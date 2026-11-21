//! # astrodev-providers
//!
//! Implementaciones del trait [`astrodev_core::Provider`].

pub mod mock;
pub mod ndjson;
pub mod ollama;

use std::sync::Arc;

use astrodev_core::config::{ProviderConfig, ProviderKind};
use astrodev_core::Provider;

pub use mock::MockProvider;
pub use ollama::OllamaProvider;

/// Crea un proveedor a partir de su configuración.
pub fn build(name: &str, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, String> {
    match cfg.kind {
        ProviderKind::Ollama => {
            Ok(Arc::new(OllamaProvider::new(name, cfg.base_url.as_deref().unwrap_or(ollama::DEFAULT_URL))))
        }
        other => Err(format!("el proveedor `{name}` ({other:?}) llegará en la fase 5; de momento usa Ollama")),
    }
}

/// Genera un id para llamadas a herramientas cuando el proveedor no lo da.
pub(crate) fn gen_call_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("call_{t:x}_{}", N.fetch_add(1, Ordering::Relaxed))
}
