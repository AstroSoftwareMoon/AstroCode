//! Proveedor Anthropic (Messages API, `POST /v1/messages` con SSE).
//!
//! - `anthropic-version: 2026-10-21`; no se envía `temperature` (esa versión lo rechaza).
//! - Los bloques del asistente (incluido el razonamiento firmado) se reenvían intactos.
//! - Todos los `tool_result` de un paso van en un único mensaje de usuario.
//! - Caché de prompt automática y, en los modelos que lo admiten, fallback del servidor
//!   ante rechazos (`fallbacks: "default"`).

use std::collections::BTreeMap;
use std::sync::Mutex;

use astrodev_core::message::{Message, Role, ToolCall};
use astrodev_core::provider::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError, StreamEvent, Usage};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::http_util::{self, client, connect_err, error_from_response};
use crate::keys::KeySource;

pub const DEFAULT_URL: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2026-10-21";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Modelos que admiten el fallback del servidor.
const FALLBACK_MODELS: &[&str] =
    &["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    key: KeySource,
    effort: Option<String>,
    http: reqwest::Client,
    ctx_cache: Mutex<BTreeMap<String, u32>>,
}

impl AnthropicProvider {
    pub fn new(name: impl Into<String>, base_url: Option<&str>, key: KeySource, effort: Option<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.unwrap_or(DEFAULT_URL).trim_end_matches('/').trim_end_matches("/v1").to_string(),
            key,
            effort,
            http: client(),
            ctx_cache: Mutex::new(BTreeMap::new()),
        }
    }

    async fn auth(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, ProviderError> {
        let key = self.key.get().await.ok_or_else(|| ProviderError::Auth {
            provider: self.name.clone(),
            body: format!("no hay clave de API (busco en {})", self.key.describe()),
        })?;
        Ok(rb.header("x-api-key", key).header("anthropic-version", API_VERSION))
    }

    fn uses_fallback(&self, model: &str) -> bool {
        self.base_url == DEFAULT_URL && FALLBACK_MODELS.contains(&model)
    }
}

fn tool_result_block(m: &Message) -> Value {
    let is_error = m.content.starts_with("ERROR:");
    let mut b =
        json!({"type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content});
    if is_error {
        b["is_error"] = json!(true);
    }
    b
}

/// Convierte el historial al formato de Anthropic: (system, messages).
pub fn to_anthropic(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let push = |role: &str, blocks: Vec<Value>, out: &mut Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        // Mensajes consecutivos del mismo rol se fusionan.
        if let Some(last) = out.last_mut() {
            if last["role"] == role {
                if let Some(arr) = last["content"].as_array_mut() {
                    arr.extend(blocks);
                    return;
                }
            }
        }
        out.push(json!({"role": role, "content": blocks}));
    };
    for m in messages {
        match m.role {
            Role::System => system.push(m.content.clone()),
            Role::User => push("user", vec![json!({"type": "text", "text": m.content})], &mut out),
            Role::Tool => push("user", vec![tool_result_block(m)], &mut out),
            Role::Assistant => {
                let blocks = if let Some(Value::Array(raw)) = m.raw_for("anthropic") {
                    raw.clone()
                } else {
                    let mut b = Vec::new();
                    if !m.content.trim().is_empty() {
                        b.push(json!({"type": "text", "text": m.content}));
                    }
                    for c in &m.tool_calls {
                        b.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments}));
                    }
                    b
                };
                push("assistant", blocks, &mut out);
            }
        }
    }
    (system.join("\n\n"), out)
}

pub fn build_body(req: &ChatRequest, effort: Option<&str>, fallback: bool) -> Value {
    let (system, messages) = to_anthropic(&req.messages);
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(32_000),
        "stream": true,
        "messages": messages,
        // Caché automática del prefijo (herramientas + sistema + historial).
        "cache_control": {"type": "ephemeral"},
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters, "eager_input_streaming": true}))
            .collect();
    }
    if let Some(e) = effort {
        body["output_config"] = json!({"effort": e});
    }
    if fallback {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Reconstruye la respuesta a partir de los eventos SSE.
#[derive(Default)]
pub struct EventParser {
    blocks: BTreeMap<u64, Value>,
    partial_json: BTreeMap<u64, String>,
    input_tokens: u64,
    cached: u64,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
}

impl EventParser {
    pub fn handle(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError> {
        let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Parse(format!("{e}: {data}")))?;
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                self.cached = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                self.input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
                    + self.cached
                    + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let i = v["index"].as_u64().unwrap_or(0);
                self.blocks.insert(i, v["content_block"].clone());
            }
            "content_block_delta" => {
                let i = v["index"].as_u64().unwrap_or(0);
                let d = &v["delta"];
                let block = self.blocks.entry(i).or_insert_with(|| json!({}));
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append(block, "text", t);
                        out.push(StreamEvent::TextDelta(t.to_string()));
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        append(block, "thinking", t);
                        if !t.is_empty() {
                            out.push(StreamEvent::ThinkingDelta(t.to_string()));
                        }
                    }
                    "signature_delta" => block["signature"] = d["signature"].clone(),
                    "input_json_delta" => {
                        self.partial_json.entry(i).or_default().push_str(d["partial_json"].as_str().unwrap_or(""));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let i = v["index"].as_u64().unwrap_or(0);
                if let Some(js) = self.partial_json.remove(&i) {
                    let input = if js.trim().is_empty() {
                        json!({})
                    } else {
                        astrodev_core::prompt_tools::lenient_json(&js).unwrap_or(json!({}))
                    };
                    if let Some(b) = self.blocks.get_mut(&i) {
                        b["input"] = input;
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
                if !v["delta"]["stop_details"].is_null() {
                    self.stop_details = Some(v["delta"]["stop_details"].clone());
                }
                let output = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                out.push(StreamEvent::Usage(Usage {
                    input_tokens: self.input_tokens,
                    output_tokens: output,
                    cached_input_tokens: self.cached,
                }));
            }
            "message_stop" => return self.finish(out).map(|_| true),
            "error" => {
                let kind = v["error"]["type"].as_str().unwrap_or("error");
                let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
                return Err(match kind {
                    "overloaded_error" | "api_error" => ProviderError::Http { status: 529, body: msg },
                    "rate_limit_error" => ProviderError::RateLimited { retry_after_secs: None },
                    _ => ProviderError::Other(format!("{kind}: {msg}")),
                });
            }
            _ => {} // ping, etc.
        }
        Ok(false)
    }

    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        match self.stop_reason.as_deref() {
            Some("refusal") => {
                let cat = self
                    .stop_details
                    .as_ref()
                    .and_then(|d| d["category"].as_str())
                    .unwrap_or("sin categoría")
                    .to_string();
                return Err(ProviderError::Refusal(cat));
            }
            Some("max_tokens") if self.blocks.values().any(|b| b["type"] == "tool_use") => {
                return Err(ProviderError::Other(
                    "la respuesta se cortó por max_tokens en mitad de una llamada a herramienta".into(),
                ));
            }
            _ => {}
        }
        let blocks: Vec<Value> = std::mem::take(&mut self.blocks).into_values().collect();
        for b in &blocks {
            if b["type"] == "tool_use" {
                out.push(StreamEvent::ToolCall(ToolCall {
                    id: b["id"].as_str().unwrap_or_default().to_string(),
                    name: b["name"].as_str().unwrap_or_default().to_string(),
                    arguments: b["input"].clone(),
                }));
            }
        }
        out.push(StreamEvent::RawAssistant(json!({"provider": "anthropic", "content": blocks})));
        Ok(())
    }
}

fn append(block: &mut Value, key: &str, t: &str) {
    let cur = block[key].as_str().unwrap_or("").to_string();
    block[key] = json!(cur + t);
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_native_tools(&self, _model: &str) -> Option<bool> {
        Some(true)
    }

    async fn context_window(&self, model: &str) -> Option<u32> {
        if let Some(c) = self.ctx_cache.lock().unwrap().get(model) {
            return Some(*c);
        }
        let rb = self.auth(self.http.get(format!("{}/v1/models/{model}", self.base_url))).await.ok()?;
        let v: Value = rb.send().await.ok()?.json().await.ok()?;
        let c = v["max_input_tokens"].as_u64()? as u32;
        self.ctx_cache.lock().unwrap().insert(model.to_string(), c);
        Some(c)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let rb = self.auth(self.http.get(format!("{}/v1/models?limit=100", self.base_url))).await?;
        let r = rb.send().await.map_err(|e| connect_err(&self.name, &self.base_url, e))?;
        if !r.status().is_success() {
            return Err(error_from_response(&self.name, r).await);
        }
        let v: Value = r.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;
        Ok(v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["id"].as_str()?.to_string(),
                            size_bytes: None,
                            context_window: m["max_input_tokens"].as_u64().map(|c| c as u32),
                            supports_tools: Some(true),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn chat_stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, ProviderError> {
        let fallback = self.uses_fallback(&request.model);
        let body = build_body(&request, self.effort.as_deref(), fallback);
        let mut rb = self.auth(self.http.post(format!("{}/v1/messages", self.base_url))).await?.json(&body);
        if fallback {
            rb = rb.header("anthropic-beta", FALLBACK_BETA);
        }
        let resp = tokio::select! {
            r = rb.send() => r.map_err(|e| connect_err(&self.name, &self.base_url, e))?,
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        if !resp.status().is_success() {
            return Err(error_from_response(&self.name, resp).await);
        }
        let mut parser = EventParser::default();
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
    fn history_conversion() {
        let c1 = ToolCall { id: "toolu_1".into(), name: "read_file".into(), arguments: json!({"path": "a"}) };
        let c2 = ToolCall { id: "toolu_2".into(), name: "grep".into(), arguments: json!({"pattern": "x"}) };
        let mut with_raw = Message::assistant_with_tools("", vec![c1.clone(), c2.clone()]);
        with_raw.raw = Some(json!({"provider": "anthropic", "content": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a"}},
            {"type": "tool_use", "id": "toolu_2", "name": "grep", "input": {"pattern": "x"}}
        ]}));
        let msgs = vec![
            Message::system("sys"),
            Message::user("hola"),
            with_raw,
            Message::tool_result(&c1, "contenido"),
            Message::tool_result(&c2, "ERROR: falló"),
        ];
        let (system, m) = to_anthropic(&msgs);
        assert_eq!(system, "sys");
        assert_eq!(m.len(), 3);
        assert_eq!(m[1]["content"][0]["signature"], "sig", "el razonamiento se reenvía intacto");
        // los dos resultados en un único mensaje de usuario
        assert_eq!(m[2]["role"], "user");
        assert_eq!(m[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(m[2]["content"][1]["is_error"], true);
    }

    #[test]
    fn body_has_no_temperature_and_uses_fallbacks() {
        let req = ChatRequest {
            model: "claude-opus-5-5".into(),
            messages: vec![Message::user("x")],
            tools: vec![ToolSpec { name: "t".into(), description: "d".into(), parameters: json!({"type":"object"}) }],
            temperature: Some(0.2),
            ..Default::default()
        };
        let b = build_body(&req, Some("high"), true);
        assert!(b.get("temperature").is_none());
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["cache_control"]["type"], "ephemeral");
    }

    fn sse(events: &[(&str, Value)]) -> String {
        events.iter().map(|(e, d)| format!("event: {e}\ndata: {d}\n\n")).collect()
    }

    #[tokio::test]
    async fn streams_text_thinking_and_tool_use() {
        let server = MockServer::start().await;
        let body = sse(&[
            (
                "message_start",
                json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90}}}),
            ),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Pienso"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}),
            ),
            ("content_block_stop", json!({"type":"content_block_stop","index":0})),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Leo el archivo."}}),
            ),
            ("content_block_stop", json!({"type":"content_block_stop","index":1})),
            (
                "content_block_start",
                json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_9","name":"read_file","input":{}}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"sr"}}),
            ),
            (
                "content_block_delta",
                json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"c/main.rs\"}"}}),
            ),
            ("content_block_stop", json!({"type":"content_block_stop","index":2})),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let p = AnthropicProvider::new(
            "anthropic",
            Some(&server.uri()),
            KeySource::fixed(Some("sk-ant-test".into())),
            None,
        );
        let mut s = p
            .chat_stream(
                ChatRequest {
                    model: "claude-sonnet-5-5".into(),
                    messages: vec![Message::user("x")],
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut raw = None;
        let mut evs = Vec::new();
        use futures::StreamExt;
        while let Some(e) = s.next().await {
            let e = e.unwrap();
            if let StreamEvent::RawAssistant(r) = &e {
                raw = Some(r.clone());
            }
            evs.push(e);
        }
        let calls: Vec<&ToolCall> =
            evs.iter().filter_map(|e| if let StreamEvent::ToolCall(c) = e { Some(c) } else { None }).collect();
        assert_eq!(calls[0].id, "toolu_9");
        assert_eq!(calls[0].arguments, json!({"path": "src/main.rs"}));
        assert!(evs.contains(&StreamEvent::ThinkingDelta("Pienso".into())));
        assert!(evs.contains(&StreamEvent::Usage(Usage {
            input_tokens: 100,
            output_tokens: 42,
            cached_input_tokens: 90
        })));
        let raw = raw.unwrap();
        assert_eq!(raw["content"][0]["signature"], "SIG");
        assert_eq!(raw["content"][2]["input"]["path"], "src/main.rs");
    }

    #[tokio::test]
    async fn refusal_and_missing_key() {
        let server = MockServer::start().await;
        let body = sse(&[
            ("message_start", json!({"type":"message_start","message":{"usage":{"input_tokens":1}}})),
            (
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}},"usage":{"output_tokens":0}}),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_string(body)).mount(&server).await;
        let p = AnthropicProvider::new("a", Some(&server.uri()), KeySource::fixed(Some("k".into())), None);
        let s = p
            .chat_stream(ChatRequest { model: "m".into(), ..Default::default() }, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(collect(s).await, Err(ProviderError::Refusal(c)) if c == "cyber"));

        let p = AnthropicProvider::new("a", Some(&server.uri()), KeySource::fixed(None), None);
        assert!(matches!(p.list_models().await, Err(ProviderError::Auth { .. })));
    }
}
