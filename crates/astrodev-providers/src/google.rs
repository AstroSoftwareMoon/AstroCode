//! Proveedor Google Gemini (`streamGenerateContent?alt=sse`).

use astrodev_core::message::{Message, Role, ToolCall};
use astrodev_core::provider::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError, StreamEvent, Usage};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::http_util::{self, client, connect_err, error_from_response};
use crate::keys::KeySource;

pub const DEFAULT_URL: &str = "https://generativelanguage.googleapis.com";

pub struct GoogleProvider {
    name: String,
    base_url: String,
    key: KeySource,
    http: reqwest::Client,
}

impl GoogleProvider {
    pub fn new(name: impl Into<String>, base_url: Option<&str>, key: KeySource) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.unwrap_or(DEFAULT_URL).trim_end_matches('/').trim_end_matches("/v1beta").to_string(),
            key,
            http: client(),
        }
    }

    async fn auth(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, ProviderError> {
        let key = self.key.get().await.ok_or_else(|| ProviderError::Auth {
            provider: self.name.clone(),
            body: format!("no hay clave de API (busco en {})", self.key.describe()),
        })?;
        Ok(rb.header("x-goog-api-key", key))
    }
}

/// Gemini acepta un subconjunto de JSON Schema: quitamos lo que no admite.
pub fn clean_schema(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "additionalProperties" | "$schema" | "default" | "examples"))
                .map(|(k, v)| (k.clone(), clean_schema(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(clean_schema).collect()),
        other => other.clone(),
    }
}

pub fn to_gemini(messages: &[Message]) -> (Option<Value>, Vec<Value>) {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let push = |role: &str, parts: Vec<Value>, out: &mut Vec<Value>| {
        if parts.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last["role"] == role {
                if let Some(arr) = last["parts"].as_array_mut() {
                    arr.extend(parts);
                    return;
                }
            }
        }
        out.push(json!({"role": role, "parts": parts}));
    };
    for m in messages {
        match m.role {
            Role::System => system.push(m.content.clone()),
            Role::User => push("user", vec![json!({"text": m.content})], &mut contents),
            Role::Tool => push(
                "user",
                vec![json!({"functionResponse": {
                    "id": m.tool_call_id,
                    "name": m.name.clone().unwrap_or_default(),
                    "response": {"content": m.content},
                }})],
                &mut contents,
            ),
            Role::Assistant => {
                let parts = if let Some(Value::Array(raw)) = m.raw_for("google") {
                    raw.clone()
                } else {
                    let mut p = Vec::new();
                    if !m.content.trim().is_empty() {
                        p.push(json!({"text": m.content}));
                    }
                    for c in &m.tool_calls {
                        p.push(json!({"functionCall": {"id": c.id, "name": c.name, "args": c.arguments}}));
                    }
                    p
                };
                push("model", parts, &mut contents);
            }
        }
    }
    let sys = (!system.is_empty()).then(|| json!({"parts": [{"text": system.join("\n\n")}]}));
    (sys, contents)
}

pub fn build_body(req: &ChatRequest) -> Value {
    let (system, contents) = to_gemini(&req.messages);
    let mut body = json!({"contents": contents});
    if let Some(s) = system {
        body["systemInstruction"] = s;
    }
    if !req.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations": req.tools.iter().map(|t| json!({
            "name": t.name, "description": t.description, "parameters": clean_schema(&t.parameters)
        })).collect::<Vec<_>>()}]);
    }
    let mut gen = serde_json::Map::new();
    if let Some(t) = req.temperature {
        gen.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_tokens {
        gen.insert("maxOutputTokens".into(), json!(n));
    }
    if !gen.is_empty() {
        body["generationConfig"] = Value::Object(gen);
    }
    body
}

#[derive(Default)]
pub struct ChunkParser {
    parts: Vec<Value>,
    calls: Vec<ToolCall>,
    usage: Option<Usage>,
}

impl ChunkParser {
    pub fn handle(&mut self, data: &str, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError> {
        let v: Value = serde_json::from_str(data).map_err(|e| ProviderError::Parse(format!("{e}: {data}")))?;
        if let Some(e) = v.get("error") {
            return Err(ProviderError::Other(e["message"].as_str().unwrap_or("error de Gemini").to_string()));
        }
        let cand = &v["candidates"][0];
        if let Some(parts) = cand["content"]["parts"].as_array() {
            for p in parts {
                if let Some(t) = p["text"].as_str() {
                    if p["thought"].as_bool() == Some(true) {
                        out.push(StreamEvent::ThinkingDelta(t.to_string()));
                    } else {
                        out.push(StreamEvent::TextDelta(t.to_string()));
                    }
                }
                let mut p = p.clone();
                if let Some(fc) = p.get_mut("functionCall") {
                    let id = fc["id"].as_str().map(str::to_string).unwrap_or_else(crate::gen_call_id);
                    fc["id"] = json!(id);
                    self.calls.push(ToolCall {
                        id,
                        name: fc["name"].as_str().unwrap_or_default().to_string(),
                        arguments: if fc["args"].is_object() { fc["args"].clone() } else { json!({}) },
                    });
                }
                self.parts.push(p);
            }
        }
        if let Some(u) = v.get("usageMetadata") {
            self.usage = Some(Usage {
                input_tokens: u["promptTokenCount"].as_u64().unwrap_or(0),
                output_tokens: u["candidatesTokenCount"].as_u64().unwrap_or(0)
                    + u["thoughtsTokenCount"].as_u64().unwrap_or(0),
                cached_input_tokens: u["cachedContentTokenCount"].as_u64().unwrap_or(0),
            });
        }
        if let Some(reason) = cand["finishReason"].as_str() {
            if matches!(reason, "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST") {
                return Err(ProviderError::Refusal(reason.to_lowercase()));
            }
        }
        Ok(false)
    }

    /// Al cerrar el stream: llamadas, uso y contenido crudo (con `thoughtSignature`).
    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) {
        out.extend(self.calls.drain(..).map(StreamEvent::ToolCall));
        if let Some(u) = self.usage.take() {
            out.push(StreamEvent::Usage(u));
        }
        // Fusionar trozos de texto consecutivos para no reenviar cientos de partes.
        let mut merged: Vec<Value> = Vec::new();
        for p in self.parts.drain(..) {
            let plain = p.as_object().is_some_and(|o| o.len() == 1 && o.contains_key("text"));
            if plain {
                if let Some(last) = merged.last_mut() {
                    if last.as_object().is_some_and(|o| o.len() == 1 && o.contains_key("text")) {
                        let t = format!("{}{}", last["text"].as_str().unwrap_or(""), p["text"].as_str().unwrap_or(""));
                        last["text"] = json!(t);
                        continue;
                    }
                }
            }
            merged.push(p);
        }
        out.push(StreamEvent::RawAssistant(json!({"provider": "google", "content": merged})));
    }
}

#[async_trait]
impl Provider for GoogleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_native_tools(&self, _model: &str) -> Option<bool> {
        Some(true)
    }

    async fn context_window(&self, model: &str) -> Option<u32> {
        let rb = self.auth(self.http.get(format!("{}/v1beta/models/{model}", self.base_url))).await.ok()?;
        let v: Value = rb.send().await.ok()?.json().await.ok()?;
        v["inputTokenLimit"].as_u64().map(|n| n as u32)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let rb = self.auth(self.http.get(format!("{}/v1beta/models?pageSize=200", self.base_url))).await?;
        let r = rb.send().await.map_err(|e| connect_err(&self.name, &self.base_url, e))?;
        if !r.status().is_success() {
            return Err(error_from_response(&self.name, r).await);
        }
        let v: Value = r.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|m| {
                        m["supportedGenerationMethods"]
                            .as_array()
                            .is_some_and(|g| g.iter().any(|x| x == "generateContent"))
                    })
                    .filter_map(|m| {
                        Some(ModelInfo {
                            id: m["name"].as_str()?.trim_start_matches("models/").to_string(),
                            size_bytes: None,
                            context_window: m["inputTokenLimit"].as_u64().map(|n| n as u32),
                            supports_tools: Some(true),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn chat_stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, ProviderError> {
        let url = format!("{}/v1beta/models/{}:streamGenerateContent?alt=sse", self.base_url, request.model);
        let rb = self.auth(self.http.post(&url)).await?.json(&build_body(&request));
        let resp = tokio::select! {
            r = rb.send() => r.map_err(|e| connect_err(&self.name, &self.base_url, e))?,
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        if !resp.status().is_success() {
            return Err(error_from_response(&self.name, resp).await);
        }
        let mut parser = ChunkParser::default();
        let mut finished = false;
        Ok(http_util::sse_stream(resp, cancel, move |ev, out| {
            if finished {
                return Ok(true);
            }
            parser.handle(&ev.data, out)?;
            // Gemini no manda un evento final propio: cerramos al ver finishReason.
            if ev.data.contains("\"finishReason\"") {
                finished = true;
                parser.finish(out);
                return Ok(true);
            }
            Ok(false)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::ToolSpec;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn conversion_and_schema_cleanup() {
        let c = ToolCall { id: "f1".into(), name: "grep".into(), arguments: json!({"pattern": "x"}) };
        let msgs = vec![
            Message::system("s"),
            Message::user("u"),
            Message::assistant_with_tools("", vec![c.clone()]),
            Message::tool_result(&c, "r"),
        ];
        let req = ChatRequest {
            model: "gemini".into(),
            messages: msgs,
            tools: vec![ToolSpec {
                name: "grep".into(),
                description: "d".into(),
                parameters: json!({"type":"object","additionalProperties":false,"properties":{"p":{"type":"string"}}}),
            }],
            ..Default::default()
        };
        let b = build_body(&req);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "s");
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "grep");
        assert_eq!(b["contents"][2]["parts"][0]["functionResponse"]["response"]["content"], "r");
        assert!(b["tools"][0]["functionDeclarations"][0]["parameters"].get("additionalProperties").is_none());
    }

    #[tokio::test]
    async fn streams_function_call_with_signature() {
        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Voy \"}]}}]}\r\n\r\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"a mirar\"}]}}]}\r\n\r\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"list_dir\",\"args\":{}},\"thoughtSignature\":\"TS\"}]},\"finishReason\":\"STOP\"}],",
            "\"usageMetadata\":{\"promptTokenCount\":20,\"candidatesTokenCount\":5}}\r\n\r\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-x:streamGenerateContent"))
            .and(header("x-goog-api-key", "g-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let p = GoogleProvider::new("google", Some(&server.uri()), KeySource::fixed(Some("g-key".into())));
        let mut s = p
            .chat_stream(
                ChatRequest { model: "gemini-x".into(), messages: vec![Message::user("x")], ..Default::default() },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        use futures::StreamExt;
        let mut evs = Vec::new();
        while let Some(e) = s.next().await {
            evs.push(e.unwrap());
        }
        let raw =
            evs.iter().find_map(|e| if let StreamEvent::RawAssistant(r) = e { Some(r.clone()) } else { None }).unwrap();
        assert_eq!(raw["content"][0]["text"], "Voy a mirar");
        assert_eq!(raw["content"][1]["thoughtSignature"], "TS");
        let call =
            evs.iter().find_map(|e| if let StreamEvent::ToolCall(c) = e { Some(c.clone()) } else { None }).unwrap();
        assert_eq!(call.name, "list_dir");
        assert_eq!(raw["content"][1]["functionCall"]["id"], json!(call.id));
        assert!(evs.contains(&StreamEvent::Usage(Usage {
            input_tokens: 20,
            output_tokens: 5,
            cached_input_tokens: 0
        })));
    }
}
