//! Ollama: instalación del runtime y API de gestión de modelos (pull/delete/ps).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use astrodev_hardware::{Backend, HardwareInfo};

use crate::runtime::{self, RuntimeStatus};
use crate::{download, extract, paths, InstallError, Reporter, Result};

pub const DEFAULT_URL: &str = "http://localhost:11434";

pub struct OllamaClient {
    base: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedModel {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
}

impl LoadedModel {
    pub fn fully_on_gpu(&self) -> bool {
        self.size_vram >= self.size.saturating_sub(self.size / 50)
    }
}

impl OllamaClient {
    pub fn new(base: &str) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), http: crate::http() }
    }

    pub async fn version(&self) -> Result<String> {
        let r = self
            .http
            .get(format!("{}/api/version", self.base))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| InstallError::Network(e.to_string()))?;
        let v: Value = r.json().await.map_err(|e| InstallError::Network(e.to_string()))?;
        Ok(v["version"].as_str().unwrap_or("?").to_string())
    }

    /// Descarga un modelo con `/api/pull` (Ollama reanuda y verifica los digests).
    pub async fn pull(&self, model: &str, rep: &Reporter, cancel: &CancellationToken) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/api/pull", self.base))
            .json(&json!({"model": model, "stream": true}))
            .send()
            .await
            .map_err(|e| InstallError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(InstallError::Other(format!("Ollama no pudo descargar {model}: {body}")));
        }
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        let start = Instant::now();
        let mut last_status = String::new();
        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.cancelled() => return Err(InstallError::Cancelled),
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| InstallError::Network(e.to_string()))?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
                if let Some(e) = v["error"].as_str() {
                    return Err(InstallError::Other(format!("Ollama: {e}")));
                }
                let status = v["status"].as_str().unwrap_or("").to_string();
                if let (Some(total), Some(done)) = (v["total"].as_u64(), v["completed"].as_u64()) {
                    let secs = start.elapsed().as_secs_f64().max(0.001);
                    rep.progress(
                        &format!("{model} · {}", short_status(&status)),
                        done,
                        Some(total),
                        done as f64 / secs,
                    );
                } else if status != last_status && !status.is_empty() {
                    rep.log(status.clone());
                }
                if status == "success" {
                    return Ok(());
                }
                last_status = status;
            }
        }
        Ok(())
    }

    pub async fn delete(&self, model: &str) -> Result<()> {
        let r = self
            .http
            .delete(format!("{}/api/delete", self.base))
            .json(&json!({"model": model}))
            .send()
            .await
            .map_err(|e| InstallError::Network(e.to_string()))?;
        if r.status().as_u16() == 404 {
            return Err(InstallError::Other(format!("el modelo {model} no está instalado")));
        }
        if !r.status().is_success() {
            return Err(InstallError::Other(r.text().await.unwrap_or_default()));
        }
        Ok(())
    }

    /// Modelos cargados en memoria (`/api/ps`): cuánto está en VRAM.
    pub async fn ps(&self) -> Result<Vec<LoadedModel>> {
        let v: Value = self
            .http
            .get(format!("{}/api/ps", self.base))
            .send()
            .await
            .map_err(|e| InstallError::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| InstallError::Network(e.to_string()))?;
        Ok(parse_ps(&v))
    }

    /// Descarga el modelo de memoria (keep_alive 0).
    pub async fn unload(&self, model: &str) {
        let _ = self
            .http
            .post(format!("{}/api/generate", self.base))
            .json(&json!({"model": model, "keep_alive": 0}))
            .send()
            .await;
    }
}

fn short_status(s: &str) -> String {
    s.strip_prefix("pulling ").map(|d| format!("capa {}", &d[..d.len().min(12)])).unwrap_or_else(|| s.to_string())
}

pub fn parse_ps(v: &Value) -> Vec<LoadedModel> {
    v["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(LoadedModel {
                        name: m["name"].as_str()?.to_string(),
                        size: m["size"].as_u64().unwrap_or(0),
                        size_vram: m["size_vram"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Cómo se instalará Ollama en esta plataforma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaInstallMethod {
    /// Descargar y extraer en el directorio de AstroDev (sin sudo).
    Archive { url: String },
    /// Usar un gestor de paquetes.
    Command { program: String, args: Vec<String> },
    /// Instalador gráfico/silencioso de Windows.
    WindowsSetup { url: String },
}

pub fn install_method(hw: &HardwareInfo) -> OllamaInstallMethod {
    let arch = if hw.arch == "aarch64" { "arm64" } else { "amd64" };
    match hw.os.as_str() {
        "macos" => {
            if paths::which("brew").is_some() {
                OllamaInstallMethod::Command { program: "brew".into(), args: vec!["install".into(), "ollama".into()] }
            } else {
                OllamaInstallMethod::Archive { url: "https://ollama.com/download/Ollama-darwin.zip".into() }
            }
        }
        "windows" => {
            if paths::which("winget").is_some() {
                OllamaInstallMethod::Command {
                    program: "winget".into(),
                    args: [
                        "install",
                        "-e",
                        "--id",
                        "Ollama.Ollama",
                        "--silent",
                        "--accept-package-agreements",
                        "--accept-source-agreements",
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                }
            } else {
                OllamaInstallMethod::WindowsSetup { url: "https://ollama.com/download/OllamaSetup.exe".into() }
            }
        }
        _ => {
            // Linux: build con ROCm para AMD; la normal ya incluye CUDA.
            let suffix = if hw.backend() == Backend::Rocm && arch == "amd64" { "-rocm" } else { "" };
            OllamaInstallMethod::Archive { url: format!("https://ollama.com/download/ollama-linux-{arch}{suffix}.tgz") }
        }
    }
}

/// Instala Ollama (si no está) y arranca el servidor.
pub async fn install(
    hw: &HardwareInfo,
    base_url: &str,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<RuntimeStatus> {
    let st = runtime::ollama_status(base_url).await;
    if st.running {
        rep.log(format!("Ollama {} ya está en marcha", st.version.clone().unwrap_or_default()));
        return Ok(st);
    }
    if st.binary.is_none() {
        let method = install_method(hw);
        rep.step(format!("Instalando Ollama ({})", hw.backend().label()));
        match method {
            OllamaInstallMethod::Archive { url } => {
                let name = url.rsplit('/').next().unwrap_or("ollama.tgz").to_string();
                let tmp = paths::runtimes_dir().join(".downloads").join(&name);
                download::download(&url, &tmp, None, "Ollama", rep, cancel).await?;
                let dest = paths::runtimes_dir().join("ollama");
                rep.log("Extrayendo…");
                let (t2, d2) = (tmp.clone(), dest.clone());
                tokio::task::spawn_blocking(move || extract::extract(&t2, &d2))
                    .await
                    .map_err(|e| InstallError::Other(e.to_string()))??;
                let _ = std::fs::remove_file(&tmp);
                let bin = runtime::ollama_managed_bin();
                if !bin.is_file() {
                    return Err(InstallError::Other(format!(
                        "no se encontró el binario tras extraer en {}",
                        dest.display()
                    )));
                }
                extract::make_executable(&bin)?;
            }
            OllamaInstallMethod::Command { program, args } => {
                rep.log(format!("$ {program} {}", args.join(" ")));
                let ok = tokio::process::Command::new(&program).args(&args).status().await?.success();
                if !ok {
                    return Err(InstallError::Other(format!("`{program}` falló al instalar Ollama")));
                }
            }
            OllamaInstallMethod::WindowsSetup { url } => {
                let tmp = paths::runtimes_dir().join(".downloads").join("OllamaSetup.exe");
                download::download(&url, &tmp, None, "Ollama", rep, cancel).await?;
                rep.log("Ejecutando el instalador de Ollama…");
                let ok = tokio::process::Command::new(&tmp).arg("/SILENT").status().await?.success();
                if !ok {
                    return Err(InstallError::Other("el instalador de Ollama terminó con error".into()));
                }
            }
        }
    }
    start_server(base_url, rep).await
}

/// Arranca `ollama serve` en segundo plano si no responde.
pub async fn start_server(base_url: &str, rep: &Reporter) -> Result<RuntimeStatus> {
    let st = runtime::ollama_status(base_url).await;
    if st.running {
        return Ok(st);
    }
    let bin: PathBuf = st.binary.clone().ok_or_else(|| InstallError::Other("Ollama no está instalado".into()))?;
    rep.step("Arrancando el servidor de Ollama");
    let host = base_url.trim_start_matches("http://").trim_start_matches("https://").to_string();
    runtime::spawn_background(&bin, &["serve".to_string()], "ollama-serve.log", &[("OLLAMA_HOST", host)])?;
    if !runtime::wait_until_up(&format!("{base_url}/api/version"), Duration::from_secs(30)).await {
        return Err(InstallError::Other(format!(
            "Ollama no respondió en 30 s; revisa {}",
            paths::logs_dir().join("ollama-serve.log").display()
        )));
    }
    Ok(runtime::ollama_status(base_url).await)
}

/// Borra el runtime si lo instaló AstroDev.
pub fn uninstall_managed() -> Result<String> {
    let dir = paths::runtimes_dir().join("ollama");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        Ok(format!("Eliminado {}", dir.display()))
    } else {
        Ok(match std::env::consts::OS {
            "macos" => "Ollama no lo instaló AstroDev. Desinstálalo con `brew uninstall ollama` o borrando Ollama.app.".into(),
            "windows" => "Ollama no lo instaló AstroDev. Desinstálalo desde Configuración → Aplicaciones.".into(),
            _ => "Ollama no lo instaló AstroDev. Si usaste el script oficial: `sudo rm /usr/local/bin/ollama` (y el servicio systemd).".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_hardware::detect::{parse_nvidia_smi, CpuInfo};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn hw(os: &str, arch: &str, gpus: Vec<astrodev_hardware::GpuInfo>) -> HardwareInfo {
        HardwareInfo {
            os: os.into(),
            arch: arch.into(),
            cpu: CpuInfo {
                brand: "x".into(),
                physical_cores: 8,
                logical_cores: 8,
                avx2: true,
                avx512: false,
                neon: false,
            },
            ram_total: 32_000_000_000,
            ram_available: 30_000_000_000,
            gpus,
            disk_free: 100_000_000_000,
        }
    }

    #[test]
    fn linux_archive_urls() {
        let m = install_method(&hw("linux", "x86_64", parse_nvidia_smi("RTX 4070, 12000, 11000, 1")));
        assert_eq!(
            m,
            OllamaInstallMethod::Archive { url: "https://ollama.com/download/ollama-linux-amd64.tgz".into() }
        );
        let amd = astrodev_hardware::GpuInfo {
            vendor: astrodev_hardware::GpuVendor::Amd,
            name: "RX 7900 XTX".into(),
            vram_total: 24_000_000_000,
            vram_free: None,
            unified: false,
            driver: None,
        };
        let m = install_method(&hw("linux", "x86_64", vec![amd]));
        assert_eq!(
            m,
            OllamaInstallMethod::Archive { url: "https://ollama.com/download/ollama-linux-amd64-rocm.tgz".into() }
        );
        let m = install_method(&hw("linux", "aarch64", vec![]));
        assert_eq!(
            m,
            OllamaInstallMethod::Archive { url: "https://ollama.com/download/ollama-linux-arm64.tgz".into() }
        );
    }

    #[tokio::test]
    async fn pull_reports_progress() {
        let server = MockServer::start().await;
        let body = concat!(
            "{\"status\":\"pulling manifest\"}\n",
            "{\"status\":\"pulling abc123\",\"digest\":\"sha256:abc\",\"total\":100,\"completed\":50}\n",
            "{\"status\":\"pulling abc123\",\"digest\":\"sha256:abc\",\"total\":100,\"completed\":100}\n",
            "{\"status\":\"verifying sha256 digest\"}\n",
            "{\"status\":\"success\"}\n"
        );
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        OllamaClient::new(&server.uri()).pull("m", &Reporter::new(tx), &CancellationToken::new()).await.unwrap();
        let mut progress = 0;
        let mut logs = Vec::new();
        while let Ok(e) = rx.try_recv() {
            match e {
                crate::InstallEvent::Progress { done, total, .. } => {
                    progress += 1;
                    assert_eq!(total, Some(100));
                    assert!(done <= 100);
                }
                crate::InstallEvent::Log(l) => logs.push(l),
                _ => {}
            }
        }
        assert_eq!(progress, 2);
        assert!(logs.iter().any(|l| l.contains("verifying")));
    }

    #[tokio::test]
    async fn pull_error_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{\"error\":\"pull model manifest: file does not exist\"}\n"),
            )
            .mount(&server)
            .await;
        let e = OllamaClient::new(&server.uri())
            .pull("x", &Reporter::silent(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("does not exist"));
    }

    #[test]
    fn ps_parsing() {
        let v = json!({"models":[{"name":"a","size":1000,"size_vram":1000},{"name":"b","size":1000,"size_vram":400}]});
        let p = parse_ps(&v);
        assert!(p[0].fully_on_gpu());
        assert!(!p[1].fully_on_gpu());
    }
}
