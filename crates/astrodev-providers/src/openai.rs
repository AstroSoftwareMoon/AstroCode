//! Proveedor compatible con la API de OpenAI (`/v1/chat/completions` con SSE).
//! Sirve para OpenAI, OpenRouter, Groq, DeepSeek, Mistral, LM Studio, llama.cpp y vLLM.

use std::collections::BTreeMap;

use astrodev_core::message::{Message, Role, ToolCall};
use astrodev_core::provider::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError, StreamEvent, Usage};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::http_util::{self, client, connect_err, error_from_response};
use crate::keys::KeySource;

pub struct OpenAiProvider {
    name: String,
    base_url: String,
    key: KeySource,
    headers: BTreeMap<String, String>,
    http: reqwest::Client,
    /// Pedir `usage` en el último trozo (`stream_options.include_usage`).
    stream_usage: bool,
}

impl OpenAiProvider {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, key: KeySource) -> Self {
        let base_url: String = base_url.into();
        // Mistral rechaza `stream_options`.
        let stream_usage = !base_url.contains("mistral.ai");
        Self {
            name: name.into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
            headers: BTreeMap::new(),
            http: client(),
            stream_usage,
        }
    }

    pub fn with_headers(mut self, h: BTreeMap<String, String>) -> Self {
        self.headers = h;
        self
    }

    async fn req(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut rb = rb;
        if let Some(k) = self.key.get().await {
            rb = rb.bearer_auth(k);
        }
        if self.base_url.contains("openrouter.ai") {
            rb = rb
                .header("HTTP-Referer", "https://github.com/AstroSoftwareMoon/AstroCode")
                .header("X-Title", "AstroDev");
        }
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        rb
    }
}

pub fn to_openai_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m.role {
            Role::System => json!({"role": "system", "content": m.content}),
            Role::User => json!({"role": "user", "content": m.content}),
            Role::Assistant => {
                let mut o = json!({"role": "assistant", "content": if m.content.is_empty() && !m.tool_calls.is_empty() { Value::Null } else { json!(m.content) }});
                if !m.tool_calls.is_empty() {
                    o["tool_calls"] = m
                        .tool_calls
                        .iter()
                        .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments.to_string()}}))
                        .collect();
                }
                o
            }
            Role::Tool => json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            }),
        })
        .collect()
}

pub fn build_body(req: &ChatRequest, stream_usage: bool) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": to_openai_messages(&req.messages),
        "stream": true,
    });
    if stream_usage {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
            .collect();
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(n) = req.max_tokens {
        body["max_tokens"] = json!(n);
    }
    body
}

/// Acumula llamadas a herramientas que llegan troceadas por índice.
#[derive(Default)]
pub struct ToolCallAccumulator {
    calls: BTreeMap<u64, (String, String, String)>,
}

impl ToolCallAccumulator {
    pub fn push(&mut self, delta: &Value) {
        let idx = delta["index"].as_u64().unwrap_or(0);
        let e = self.calls.entry(idx).or_default();
        if let Some(id) = delta["id"].as_str() {
            e.0 = id.to_string();
        }
        if let Some(n) = delta.pointer("/function/name").and_then(|v| v.as_str()) {
            e.1.push_str(n);
        }
        if let Some(a) = delta.pointer("/function/arguments").and_then(|v| v.as_str()) {
            e.2.push_str(a);
        }
    }

    pub fn finish(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.calls)
            .into_values()
            .filter(|(_, n, _)| !n.is_empty())
            .map(|(id, name, args)| {
                let arguments = if args.trim().is_empty() {
                    json!({})
                } else {
                    astrodev_core::prompt_tools::lenient_json(&args).unwrap_or(Value::String(args))
                };
                ToolCall { id: if id.is_empty() { crate::gen_call_id() } else { id }, name, arguments }
            })
            .collect()
    }
}

/// Estado del parser de chunks.
#[derive(Default)]
pub struct ChunkParser {
    tools: ToolCallAccumulator,
    finished: bool,
}

impl ChunkParser {
    /// Procesa el `data:` de un evento. Devuelve `true` al terminar.
    pub fn handle(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError> {
        if data.trim() == "[DONE]" {
            out.extend(self.tools.finish().into_iter().map(StreamEvent::ToolCall));
            return Ok(true);
        }
        let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Parse(format!("{e}: {data}")))?;
        if let Some(err) = v.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("error del proveedor");
            return Err(ProviderError::Other(msg.to_string()));
        }
        if let Some(choice) = v["choices"].get(0) {
            let d = &choice["delta"];
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = d[key].as_str().filter(|t| !t.is_empty()) {
                    out.push(StreamEvent::ThinkingDelta(t.to_string()));
                }
            }
            if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
                out.push(StreamEvent::TextDelta(t.to_string()));
            }
            if let Some(tcs) = d["tool_calls"].as_array() {
                for tc in tcs {
                    self.tools.push(tc);
                }
            }
            if choice["finish_reason"].is_string() && !self.finished {
                self.finished = true;
                out.extend(self.tools.finish().into_iter().map(StreamEvent::ToolCall));
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            out.push(StreamEvent::Usage(Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                cached_input_tokens: u
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .or_else(|| u.get("prompt_cache_hit_tokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            }));
        }
        // llama.cpp informa de la velocidad.
        if let Some(tps) = v.pointer("/timings/predicted_per_second").and_then(|x| x.as_f64()) {
            out.push(StreamEvent::Stats { tokens_per_second: tps });
        }
        Ok(false)
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn context_window(&self, model: &str) -> Option<u32> {
        // llama.cpp expone el contexto con el que arrancó.
        if http_util::is_local(&self.base_url) {
            let root = self.base_url.trim_end_matches("/v1");
            if let Ok(r) = self.http.get(format!("{root}/props")).send().await {
                if let Ok(v) = r.json::<Value>().await {
                    if let Some(n) = v.pointer("/default_generation_settings/n_ctx").and_then(|x| x.as_u64()) {
                        return Some(n as u32);
                    }
                }
            }
            return None;
        }
        if let Ok(ms) = self.list_models().await {
            if let Some(c) = ms.iter().find(|m| m.id == model).and_then(|m| m.context_window) {
                return Some(c);
            }
        }
        Some(128_000)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let r =
            self.req(self.http.get(&url)).await.send().await.map_err(|e| connect_err(&self.name, &self.base_url, e))?;
        if !r.status().is_success() {
            return Err(error_from_response(&self.name, r).await);
        }
        let v: Value = r.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;
        let mut models: Vec<ModelInfo> = v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["id"].as_str()?.to_string(),
                            size_bytes: None,
                            context_window: m["context_length"].as_u64().map(|c| c as u32),
                            supports_tools: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    async fn chat_stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = build_body(&request, self.stream_usage);
        let send = self.req(self.http.post(&url)).await.json(&body).send();
        let resp = tokio::select! {
            r = send => r.map_err(|e| connect_err(&self.name, &self.base_url, e))?,
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        if !resp.status().is_success() {
            return Err(error_from_response(&self.name, resp).await);
        }
        let mut parser = ChunkParser::default();
        Ok(http_util::sse_stream(resp, cancel, move |ev, out| parser.handle(&ev.data, out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::provider::collect;
    use astrodev_core::ToolSpec;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn body_and_messages() {
        let call = ToolCall { id: "c1".into(), name: "grep".into(), arguments: json!({"pattern": "x"}) };
        let req = ChatRequest {
            model: "gpt".into(),
            messages: vec![
                Message::system("s"),
                Message::user("u"),
                Message::assistant_with_tools("", vec![call.clone()]),
                Message::tool_result(&call, "r"),
            ],
            tools: vec![ToolSpec {
                name: "grep".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            }],
            temperature: Some(0.2),
            max_tokens: Some(100),
            context_window: None,
        };
        let b = build_body(&req, true);
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["messages"][2]["content"], Value::Null);
        assert_eq!(b["messages"][2]["tool_calls"][0]["function"]["arguments"], "{\"pattern\":\"x\"}");
        assert_eq!(b["messages"][3]["role"], "tool");
        assert_eq!(b["messages"][3]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["function"]["name"], "grep");
    }

    #[test]
    fn chunked_tool_calls_and_usage() {
        let mut p = ChunkParser::default();
        let mut out = Vec::new();
        for d in [
            r#"{"choices":[{"delta":{"role":"assistant","content":"Busco"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"grep","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pat"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"tern\":\"fn\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"pienso"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":20}}}"#,
        ] {
            assert!(!p.handle(d, &mut out).unwrap());
        }
        assert!(p.handle("[DONE]", &mut out).unwrap());
        assert!(out.contains(&StreamEvent::TextDelta("Busco".into())));
        assert!(out.contains(&StreamEvent::ThinkingDelta("pienso".into())));
        let calls: Vec<&ToolCall> =
            out.iter().filter_map(|e| if let StreamEvent::ToolCall(c) = e { Some(c) } else { None }).collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].arguments, json!({"pattern": "fn"}));
        assert!(out.contains(&StreamEvent::Usage(Usage {
            input_tokens: 50,
            output_tokens: 9,
            cached_input_tokens: 20
        })));
    }

    #[tokio::test]
    async fn streams_with_bearer_auth() {
        let server = MockServer::start().await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hola\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" mundo\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(body),
            )
            .mount(&server)
            .await;
        let p = OpenAiProvider::new("groq", format!("{}/v1", server.uri()), KeySource::fixed(Some("sk-test".into())));
        let s = p
            .chat_stream(
                ChatRequest { model: "m".into(), messages: vec![Message::user("hi")], ..Default::default() },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(collect(s).await.unwrap().text, "Hola mundo");
    }

    #[tokio::test]
    async fn auth_and_rate_limit_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"Invalid API key"}}"#))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .mount(&server)
            .await;
        let p = OpenAiProvider::new("x", format!("{}/v1", server.uri()), KeySource::fixed(Some("k".into())));
        assert!(matches!(p.list_models().await, Err(ProviderError::Auth { .. })));
        let e = p.chat_stream(ChatRequest::default(), CancellationToken::new()).await.err().unwrap();
        assert!(matches!(e, ProviderError::RateLimited { retry_after_secs: Some(7) }));
    }
}
