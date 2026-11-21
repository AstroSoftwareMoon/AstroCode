//! Detección de runtimes locales.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Ollama,
    LlamaCpp,
}

impl RuntimeKind {
    pub fn label(self) -> &'static str {
        match self {
            RuntimeKind::Ollama => "Ollama",
            RuntimeKind::LlamaCpp => "llama.cpp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().replace(['.', '-', '_'], "").as_str() {
            "ollama" => Some(Self::Ollama),
            "llamacpp" | "llama" | "llamaserver" => Some(Self::LlamaCpp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub kind: RuntimeKind,
    /// Binario encontrado (PATH o instalado por AstroDev).
    pub binary: Option<PathBuf>,
    /// ¿Lo instaló AstroDev (y por tanto puede desinstalarlo)?
    pub managed: bool,
    /// ¿El servidor responde?
    pub running: bool,
    pub version: Option<String>,
}

pub fn ollama_managed_bin() -> PathBuf {
    let root = paths::runtimes_dir().join("ollama");
    if cfg!(target_os = "macos") {
        root.join("Ollama.app/Contents/Resources").join(paths::exe("ollama"))
    } else {
        root.join("bin").join(paths::exe("ollama"))
    }
}

pub fn llamacpp_managed_dir() -> PathBuf {
    paths::runtimes_dir().join("llama.cpp")
}

pub fn llamacpp_managed_bin() -> Option<PathBuf> {
    crate::extract::find_file(&llamacpp_managed_dir(), &paths::exe("llama-server"))
}

fn ollama_system_bin() -> Option<PathBuf> {
    paths::which("ollama").or_else(|| {
        let candidates: &[&str] = if cfg!(target_os = "macos") {
            &["/Applications/Ollama.app/Contents/Resources/ollama", "/usr/local/bin/ollama", "/opt/homebrew/bin/ollama"]
        } else if cfg!(windows) {
            &[]
        } else {
            &["/usr/local/bin/ollama", "/usr/bin/ollama"]
        };
        let mut v: Vec<PathBuf> = candidates.iter().map(PathBuf::from).collect();
        if cfg!(windows) {
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                v.push(PathBuf::from(local).join("Programs/Ollama/ollama.exe"));
            }
        }
        v.into_iter().find(|p| p.is_file())
    })
}

pub async fn ollama_status(base_url: &str) -> RuntimeStatus {
    let managed_bin = ollama_managed_bin();
    let (binary, managed) =
        if managed_bin.is_file() { (Some(managed_bin), true) } else { (ollama_system_bin(), false) };
    let version = crate::ollama::OllamaClient::new(base_url).version().await.ok();
    RuntimeStatus { kind: RuntimeKind::Ollama, binary, managed, running: version.is_some(), version }
}

pub async fn llamacpp_status(base_url: &str) -> RuntimeStatus {
    let managed = llamacpp_managed_bin();
    let (binary, is_managed) = match managed {
        Some(b) => (Some(b), true),
        None => (paths::which("llama-server"), false),
    };
    let running = crate::http()
        .get(format!("{}/health", base_url.trim_end_matches('/').trim_end_matches("/v1")))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    RuntimeStatus { kind: RuntimeKind::LlamaCpp, binary, managed: is_managed, running, version: None }
}

/// Espera a que una URL responda (servidor arrancando).
pub async fn wait_until_up(url: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let client = crate::http();
    while start.elapsed() < timeout {
        if let Ok(r) = client.get(url).timeout(Duration::from_secs(2)).send().await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    false
}

/// Lanza un proceso en segundo plano con la salida a un log.
pub fn spawn_background(
    bin: &std::path::Path,
    args: &[String],
    log_name: &str,
    env: &[(&str, String)],
) -> std::io::Result<u32> {
    std::fs::create_dir_all(paths::logs_dir())?;
    let log = std::fs::File::create(paths::logs_dir().join(log_name))?;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // que sobreviva a Ctrl+C en la TUI
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    Ok(cmd.spawn()?.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind() {
        assert_eq!(RuntimeKind::parse("llama.cpp"), Some(RuntimeKind::LlamaCpp));
        assert_eq!(RuntimeKind::parse("Ollama"), Some(RuntimeKind::Ollama));
        assert_eq!(RuntimeKind::parse("vllm"), None);
    }
}
