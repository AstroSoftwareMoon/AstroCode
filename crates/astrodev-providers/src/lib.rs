//! # astrodev-providers
//!
//! Implementaciones del trait [`astrodev_core::Provider`].

pub mod anthropic;
pub mod google;
pub mod http_util;
pub mod keys;
pub mod mock;
pub mod ndjson;
pub mod ollama;
pub mod openai;
pub mod presets;
pub mod pricing;
pub mod retry;
pub mod sse;

use std::sync::Arc;

use astrodev_core::config::{ProviderConfig, ProviderKind};
use astrodev_core::Provider;

pub use mock::MockProvider;
pub use ollama::OllamaProvider;

/// Crea un proveedor a partir de su configuración. Los remotos llevan reintentos.
pub fn build(name: &str, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, String> {
    let key = || keys::KeySource::new(cfg.api_key_ref.clone(), presets::env_key_for(cfg));
    let p: Arc<dyn Provider> = match cfg.kind {
        ProviderKind::Ollama => {
            return Ok(Arc::new(OllamaProvider::new(name, cfg.base_url.as_deref().unwrap_or(ollama::DEFAULT_URL))));
        }
        ProviderKind::Openai => {
            let url = cfg.base_url.clone().ok_or_else(|| format!("el proveedor `{name}` necesita base_url"))?;
            Arc::new(openai::OpenAiProvider::new(name, url, key()).with_headers(cfg.headers.clone()))
        }
        ProviderKind::Anthropic => {
            Arc::new(anthropic::AnthropicProvider::new(name, cfg.base_url.as_deref(), key(), cfg.effort.clone()))
        }
        ProviderKind::Google => Arc::new(google::GoogleProvider::new(name, cfg.base_url.as_deref(), key())),
    };
    if cfg.base_url.as_deref().is_some_and(http_util::is_local) {
        Ok(p)
    } else {
        Ok(Arc::new(retry::RetryProvider::new(p)))
    }
}

/// Genera un id para llamadas a herramientas cuando el proveedor no lo da.
pub(crate) fn gen_call_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("call_{t:x}_{}", N.fetch_add(1, Ordering::Relaxed))
}
