//! Proveedor falso guionizado para tests de integración (sin red).

use std::collections::VecDeque;
use std::sync::Mutex;

use astrodev_core::message::ToolCall;
use astrodev_core::provider::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError, StreamEvent, Usage};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Cada llamada a `chat_stream` consume el siguiente guion de eventos.
/// Si se acaban los guiones, responde con un eco del último mensaje del usuario.
pub struct MockProvider {
    name: String,
    scripts: Mutex<VecDeque<Vec<StreamEvent>>>,
    requests: Mutex<Vec<ChatRequest>>,
    native_tools: bool,
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            name: "mock".into(),
            scripts: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            native_tools: true,
        }
    }

    pub fn without_native_tools(mut self) -> Self {
        self.native_tools = false;
        self
    }

    /// Añade un guion de eventos crudo.
    pub fn push_script(&self, events: Vec<StreamEvent>) -> &Self {
        self.scripts.lock().unwrap().push_back(events);
        self
    }

    /// Atajo: respuesta de texto (troceada palabra a palabra para simular streaming).
    pub fn push_text(&self, text: &str) -> &Self {
        let mut evs: Vec<StreamEvent> =
            text.split_inclusive(' ').map(|w| StreamEvent::TextDelta(w.to_string())).collect();
        evs.push(StreamEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: text.split_whitespace().count() as u64,
            ..Default::default()
        }));
        evs.push(StreamEvent::Done);
        self.push_script(evs)
    }

    /// Atajo: respuesta con una llamada a herramienta.
    pub fn push_tool_call(&self, name: &str, args: serde_json::Value) -> &Self {
        self.push_script(vec![
            StreamEvent::ToolCall(ToolCall { id: crate::gen_call_id(), name: name.into(), arguments: args }),
            StreamEvent::Done,
        ])
    }

    /// Peticiones recibidas (para asserts).
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_native_tools(&self, _model: &str) -> Option<bool> {
        Some(self.native_tools)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![ModelInfo {
            id: "mock-model".into(),
            size_bytes: None,
            context_window: Some(8192),
            supports_tools: Some(self.native_tools),
        }])
    }

    async fn chat_stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, ProviderError> {
        let echo = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == astrodev_core::Role::User)
            .map(|m| format!("eco: {}", m.content))
            .unwrap_or_else(|| "eco".into());
        self.requests.lock().unwrap().push(request);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta(echo), StreamEvent::Done]);
        let stream = futures::stream::iter(events.into_iter().map(move |e| {
            if cancel.is_cancelled() {
                Err(ProviderError::Cancelled)
            } else {
                Ok(e)
            }
        }));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::provider::collect;
    use astrodev_core::Message;

    #[tokio::test]
    async fn scripted_then_echo() {
        let m = MockProvider::new();
        m.push_text("hola mundo");
        let req = ChatRequest { model: "x".into(), messages: vec![Message::user("ping")], ..Default::default() };
        let r = collect(m.chat_stream(req.clone(), CancellationToken::new()).await.unwrap()).await.unwrap();
        assert_eq!(r.text, "hola mundo");
        let r = collect(m.chat_stream(req, CancellationToken::new()).await.unwrap()).await.unwrap();
        assert_eq!(r.text, "eco: ping");
        assert_eq!(m.requests().len(), 2);
    }
}
