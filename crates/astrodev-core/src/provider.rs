//! Capa abstracta de proveedores de modelos.

use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::message::{Message, ToolCall};
use crate::tool::ToolSpec;

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Tamaño de contexto deseado (Ollama: `num_ctx`).
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

/// Eventos emitidos durante el streaming de una respuesta.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    /// Razonamiento del modelo (si el proveedor lo expone).
    ThinkingDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    /// Métricas de velocidad cuando el proveedor las da.
    Stats {
        tokens_per_second: f64,
    },
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no se pudo conectar con {provider} en {url}: {source}")]
    Connect {
        provider: String,
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("límite de peticiones alcanzado")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("respuesta inválida del proveedor: {0}")]
    Parse(String),
    #[error("modelo no encontrado: {0}")]
    ModelNotFound(String),
    #[error("cancelado por el usuario")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// ¿Merece la pena reintentar con backoff?
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::Connect { .. } | ProviderError::RateLimited { .. } => true,
            ProviderError::Http { status, .. } => *status >= 500 || *status == 408,
            _ => false,
        }
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub size_bytes: Option<u64>,
    pub context_window: Option<u32>,
    pub supports_tools: Option<bool>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Nombre de la instancia (clave en la configuración).
    fn name(&self) -> &str;

    /// ¿Admite tool calling nativo para este modelo? `None` = desconocido.
    fn supports_native_tools(&self, _model: &str) -> Option<bool> {
        None
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Inicia una respuesta en streaming. Al cancelar `cancel`, el stream debe terminar
    /// con `ProviderError::Cancelled` lo antes posible.
    async fn chat_stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}

/// Respuesta completa acumulada a partir de un stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectedResponse {
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

/// Consume un stream entero. Útil para tests, resúmenes y modo no interactivo.
pub async fn collect(mut stream: EventStream) -> Result<CollectedResponse, ProviderError> {
    let mut out = CollectedResponse::default();
    while let Some(ev) = stream.next().await {
        match ev? {
            StreamEvent::TextDelta(t) => out.text.push_str(&t),
            StreamEvent::ThinkingDelta(t) => out.thinking.push_str(&t),
            StreamEvent::ToolCall(c) => out.tool_calls.push(c),
            StreamEvent::Usage(u) => out.usage.add(u),
            StreamEvent::Stats { .. } => {}
            StreamEvent::Done => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(ProviderError::Http {
            status: 503,
            body: String::new()
        }
        .is_retryable());
        assert!(!ProviderError::Http {
            status: 400,
            body: String::new()
        }
        .is_retryable());
        assert!(ProviderError::RateLimited {
            retry_after_secs: None
        }
        .is_retryable());
        assert!(!ProviderError::Cancelled.is_retryable());
    }
}
