//! # astrodev-installer
//!
//! - Detecta e instala runtimes locales (Ollama o llama.cpp con la build adecuada
//!   para CUDA / ROCm / Metal / Vulkan / CPU).
//! - Descarga modelos con progreso, reanudación, verificación SHA-256 y control de
//!   espacio libre.
//! - Test de humo tras instalar (prompt de prueba + tok/s) y ajuste automático de
//!   contexto y capas en GPU.
//! - Desinstalación de modelos y runtimes gestionados por AstroDev.

pub mod download;
pub mod events;
pub mod extract;
pub mod llamacpp;
pub mod ollama;
pub mod paths;
pub mod pipeline;
pub mod runtime;
pub mod smoke;

pub use events::{InstallEvent, Reporter};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("error de red: {0}")]
    Network(String),
    #[error("HTTP {status} al descargar {url}")]
    Http { status: u16, url: String },
    #[error("espacio insuficiente: se necesitan {needed} y quedan {available}")]
    NoSpace { needed: String, available: String },
    #[error("el hash SHA-256 no coincide (esperado {expected}, obtenido {actual}); el archivo se ha borrado")]
    HashMismatch { expected: String, actual: String },
    #[error("cancelado")]
    Cancelled,
    #[error("E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, InstallError>;

pub(crate) fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

pub(crate) fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("astrodev/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("cliente HTTP")
}
