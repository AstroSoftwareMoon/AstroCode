//! Eventos de progreso para la TUI y la CLI.

use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum InstallEvent {
    /// Comienza un paso (p.ej. "Descargando llama.cpp").
    Step(String),
    /// Progreso de una descarga.
    Progress {
        label: String,
        done: u64,
        total: Option<u64>,
        bytes_per_sec: f64,
    },
    Log(String),
    /// Terminado con éxito (resumen).
    Done(String),
    Error(String),
}

/// Envoltorio cómodo sobre un canal de eventos (o nada).
#[derive(Clone, Default)]
pub struct Reporter {
    tx: Option<mpsc::UnboundedSender<InstallEvent>>,
}

impl Reporter {
    pub fn new(tx: mpsc::UnboundedSender<InstallEvent>) -> Self {
        Self { tx: Some(tx) }
    }

    pub fn silent() -> Self {
        Self { tx: None }
    }

    pub fn send(&self, e: InstallEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(e);
        }
    }

    pub fn step(&self, s: impl Into<String>) {
        self.send(InstallEvent::Step(s.into()));
    }

    pub fn log(&self, s: impl Into<String>) {
        self.send(InstallEvent::Log(s.into()));
    }

    pub fn progress(&self, label: &str, done: u64, total: Option<u64>, bytes_per_sec: f64) {
        self.send(InstallEvent::Progress { label: label.to_string(), done, total, bytes_per_sec });
    }
}
