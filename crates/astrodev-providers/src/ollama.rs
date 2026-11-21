//! Proveedor Ollama (`/api/chat` con streaming NDJSON y tool calling nativo).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use astrodev_core::message::{Message, Role, ToolCall};
use astrodev_core::provider::{
    ChatRequest, EventStream, ModelInfo, Provider, ProviderError, StreamEvent, Usage,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::ndjson::LineDecoder;

pub const DEFAULT_URL: &str = "http://localhost:11434";

pub struct OllamaProvider {
    name: String,
    base_url: String,
    http: reqwest::Client,
    /// Cache de capacidades por modelo (de `/api/show`).
    caps: Mutex<HashMap<String, ModelCaps>>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCaps {
    pub tools: Option<bool>,
    pub context_length: Option<u32>,
}

impl OllamaProvider {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("cliente HTTP");
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
            caps: Mutex::new(HashMap::new()),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn connect_err(&self, e: reqwest::Error) -> ProviderError {
        ProviderError::Connect {
            provider: format!(
                "Ollama ({}) — ¿está en marcha? prueba `ollama serve`",
                self.name
            ),
            url: self.base_url.clone(),
            source: Box::new(e),
        }
    }

    /// ¿Responde el servidor? Devuelve la versión.
    pub async fn version(&self) -> Result<String, ProviderError> {
        let r = self
            .http
            .get(format!("{}/api/version", self.base_url))
            .send()
            .await
            .map_err(|e| self.connect_err(e))?;
        let v: Value = r
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;
        Ok(v["version"].as_str().unwrap_or("?").to_string())
    }

    /// Consulta `/api/show` para conocer capacidades (tools) y longitud de contexto.
    pub async fn probe(&self, model: &str) -> Result<ModelCaps, ProviderError> {
        if let Some(c) = self.caps.lock().unwrap().get(model) {
            return Ok(c.clone());
        }
        let r = self
            .http
            .post(format!("{}/api/show", self.base_url))
            .json(&json!({ "model": model }))
            .send()
            .await
            .map_err(|e| self.connect_err(e))?;
        if r.status().as_u16() == 404 {
            return Err(ProviderError::ModelNotFound(model.to_string()));
        }
        let v: Value = r
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let caps = parse_show(&v);
        self.caps
            .lock()
            .unwrap()
            .insert(model.to_string(), caps.clone());
        Ok(caps)
    }
}

pub(crate) fn parse_show(v: &Value) -> ModelCaps {
    let tools = v["capabilities"]
        .as_array()
        .map(|a| a.iter().any(|c| c.as_str() == Some("tools")));
    let context_length = v["model_info"].as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
            .map(|n| n as u32)
    });
    ModelCaps {
        tools,
        context_length,
    }
}

/// Convierte mensajes al formato de Ollama.
pub(crate) fn to_ollama_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut o = json!({ "role": role, "content": m.content });
            if !m.tool_calls.is_empty() {
                o["tool_calls"] = m
                    .tool_calls
                    .iter()
                    .map(|c| json!({"function": {"name": c.name, "arguments": c.arguments}}))
                    .collect();
            }
            if m.role == Role::Tool {
                if let Some(n) = &m.name {
                    o["tool_name"] = json!(n);
                }
            }
            o
        })
        .collect()
}

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": to_ollama_messages(&req.messages),
        "stream": true,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters
                }})
            })
            .collect();
    }
    let mut options = serde_json::Map::new();
    if let Some(t) = req.temperature {
        options.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.context_window {
        options.insert("num_ctx".into(), json!(n));
    }
    if let Some(n) = req.max_tokens {
        options.insert("num_predict".into(), json!(n));
    }
    if !options.is_empty() {
        body["options"] = Value::Object(options);
    }
    body
}

#[derive(Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChunkToolCall>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    function: ChunkFunction,
}

#[derive(Deserialize)]
struct ChunkFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    message: Option<ChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    eval_duration: Option<u64>,
}

/// Traduce una línea NDJSON de Ollama a eventos.
pub(crate) fn parse_line(line: &str) -> Result<Vec<StreamEvent>, ProviderError> {
    let chunk: Chunk =
        serde_json::from_str(line).map_err(|e| ProviderError::Parse(format!("{e}: {line}")))?;
    if let Some(err) = chunk.error {
        return Err(map_error_text(&err));
    }
    let mut out = Vec::new();
    if let Some(m) = chunk.message {
        if let Some(t) = m.thinking.filter(|t| !t.is_empty()) {
            out.push(StreamEvent::ThinkingDelta(t));
        }
        if !m.content.is_empty() {
            out.push(StreamEvent::TextDelta(m.content));
        }
        for c in m.tool_calls {
            // Algunos modelos devuelven los argumentos como string JSON.
            let arguments = match c.function.arguments {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                Value::Null => json!({}),
                v => v,
            };
            out.push(StreamEvent::ToolCall(ToolCall {
                id: crate::gen_call_id(),
                name: c.function.name,
                arguments,
            }));
        }
    }
    if chunk.done {
        if chunk.prompt_eval_count.is_some() || chunk.eval_count.is_some() {
            out.push(StreamEvent::Usage(Usage {
                input_tokens: chunk.prompt_eval_count.unwrap_or(0),
                output_tokens: chunk.eval_count.unwrap_or(0),
            }));
        }
        if let (Some(n), Some(d)) = (chunk.eval_count, chunk.eval_duration) {
            if d > 0 {
                out.push(StreamEvent::Stats {
                    tokens_per_second: n as f64 / (d as f64 / 1e9),
                });
            }
        }
        out.push(StreamEvent::Done);
    }
    Ok(out)
}

fn map_error_text(err: &str) -> ProviderError {
    let lower = err.to_lowercase();
    if lower.contains("not found") && lower.contains("model") {
        ProviderError::ModelNotFound(err.to_string())
    } else {
        ProviderError::Other(format!("Ollama: {err}"))
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_native_tools(&self, model: &str) -> Option<bool> {
        self.caps.lock().unwrap().get(model).and_then(|c| c.tools)
    }

    async fn probe_native_tools(&self, model: &str) -> Option<bool> {
        self.probe(model).await.ok().and_then(|c| c.tools)
    }

    async fn context_window(&self, model: &str) -> Option<u32> {
        self.probe(model).await.ok().and_then(|c| c.context_length)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let r = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| self.connect_err(e))?;
        if !r.status().is_success() {
            return Err(ProviderError::Http {
                status: r.status().as_u16(),
                body: r.text().await.unwrap_or_default(),
            });
        }
        let v: Value = r
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;
        let models = v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["name"].as_str()?.to_string(),
                            size_bytes: m["size"].as_u64(),
                            context_window: None,
                            supports_tools: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let body = build_body(&request);
        tracing::debug!(model = %request.model, msgs = request.messages.len(), "ollama chat");
        let send = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send();
        let resp = tokio::select! {
            r = send => r.map_err(|e| self.connect_err(e))?,
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(text);
            if status.as_u16() == 404 {
                return Err(ProviderError::ModelNotFound(format!(
                    "{} ({msg}). Descárgalo con `ollama pull {}`",
                    request.model, request.model
                )));
            }
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: msg,
            });
        }

        let (tx, mut rx) = mpsc::channel::<Result<StreamEvent, ProviderError>>(64);
        let mut bytes = resp.bytes_stream();
        tokio::spawn(async move {
            let mut dec = LineDecoder::new();
            let mut done = false;
            loop {
                let chunk = tokio::select! {
                    c = bytes.next() => c,
                    _ = cancel.cancelled() => {
                        let _ = tx.send(Err(ProviderError::Cancelled)).await;
                        return;
                    }
                };
                let eof = chunk.is_none();
                let lines = match chunk {
                    Some(Ok(b)) => dec.push(&b),
                    Some(Err(e)) => {
                        let _ = tx
                            .send(Err(ProviderError::Other(format!("stream cortado: {e}"))))
                            .await;
                        return;
                    }
                    None => dec.finish().into_iter().collect(),
                };
                for line in lines {
                    match parse_line(&line) {
                        Ok(evs) => {
                            for ev in evs {
                                done |= ev == StreamEvent::Done;
                                if tx.send(Ok(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }
                if done {
                    return;
                }
                if eof {
                    // El servidor cerró sin `done: true`.
                    let _ = tx.send(Ok(StreamEvent::Done)).await;
                    return;
                }
            }
        });
        Ok(Box::pin(futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::provider::collect;
    use astrodev_core::ToolSpec;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_text_and_done() {
        let evs =
            parse_line(r#"{"message":{"role":"assistant","content":"Ho"},"done":false}"#).unwrap();
        assert_eq!(evs, vec![StreamEvent::TextDelta("Ho".into())]);
        let evs = parse_line(
            r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":10,"eval_count":20,"eval_duration":1000000000}"#,
        )
        .unwrap();
        assert_eq!(
            evs,
            vec![
                StreamEvent::Usage(Usage {
                    input_tokens: 10,
                    output_tokens: 20
                }),
                StreamEvent::Stats {
                    tokens_per_second: 20.0
                },
                StreamEvent::Done
            ]
        );
    }

    #[test]
    fn parses_tool_calls_with_string_args() {
        let evs = parse_line(
            r#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"done":false}"#,
        )
        .unwrap();
        match &evs[0] {
            StreamEvent::ToolCall(c) => {
                assert_eq!(c.name, "read_file");
                assert_eq!(c.arguments["path"], "a.rs");
                assert!(!c.id.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn error_line() {
        assert!(matches!(
            parse_line(r#"{"error":"model 'x' not found"}"#),
            Err(ProviderError::ModelNotFound(_))
        ));
    }

    #[test]
    fn body_includes_tools_and_options() {
        let call = ToolCall {
            id: "1".into(),
            name: "glob".into(),
            arguments: json!({"pattern":"*"}),
        };
        let req = ChatRequest {
            model: "qwen".into(),
            messages: vec![
                Message::user("hola"),
                Message::assistant_with_tools("", vec![call.clone()]),
                Message::tool_result(&call, "a.rs"),
            ],
            tools: vec![ToolSpec {
                name: "glob".into(),
                description: "d".into(),
                parameters: json!({}),
            }],
            temperature: Some(0.1),
            context_window: Some(8192),
            max_tokens: None,
        };
        let b = build_body(&req);
        assert_eq!(b["tools"][0]["function"]["name"], "glob");
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["name"],
            "glob"
        );
        assert_eq!(b["messages"][2]["role"], "tool");
        assert_eq!(b["messages"][2]["tool_name"], "glob");
    }

    #[test]
    fn show_parsing() {
        let v = json!({"capabilities":["completion","tools"],"model_info":{"qwen2.context_length":32768}});
        let c = parse_show(&v);
        assert_eq!(c.tools, Some(true));
        assert_eq!(c.context_length, Some(32768));
    }

    #[tokio::test]
    async fn streams_from_server() {
        let server = MockServer::start().await;
        let body = concat!(
            r#"{"message":{"role":"assistant","content":"Hola"},"done":false}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":" mundo"},"done":false}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":3,"eval_count":2}"#,
            "\n",
        );
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let p = OllamaProvider::new("ollama", server.uri());
        let s = p
            .chat_stream(
                ChatRequest {
                    model: "m".into(),
                    messages: vec![Message::user("hi")],
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let r = collect(s).await.unwrap();
        assert_eq!(r.text, "Hola mundo");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 3,
                output_tokens: 2
            }
        );
    }

    #[tokio::test]
    async fn model_not_found_404() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_string(r#"{"error":"model \"x\" not found, try pulling it first"}"#),
            )
            .mount(&server)
            .await;
        let p = OllamaProvider::new("ollama", server.uri());
        let r = p
            .chat_stream(
                ChatRequest {
                    model: "x".into(),
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await;
        match r {
            Err(ProviderError::ModelNotFound(m)) => assert!(m.contains("ollama pull x")),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("esperaba error"),
        }
    }

    #[tokio::test]
    async fn lists_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"models":[{"name":"qwen2.5-coder:7b","size":4683087332u64}]}),
            ))
            .mount(&server)
            .await;
        let p = OllamaProvider::new("ollama", server.uri());
        let m = p.list_models().await.unwrap();
        assert_eq!(m[0].id, "qwen2.5-coder:7b");
        assert_eq!(m[0].size_bytes, Some(4683087332));
    }

    #[tokio::test]
    async fn connection_refused_is_connect_error() {
        let p = OllamaProvider::new("ollama", "http://127.0.0.1:9");
        let e = p.list_models().await.unwrap_err();
        assert!(matches!(e, ProviderError::Connect { .. }));
        assert!(e.is_retryable());
        assert!(e.to_string().contains("ollama serve"));
    }
}
