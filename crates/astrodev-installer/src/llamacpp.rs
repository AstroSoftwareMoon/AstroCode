//! llama.cpp: descarga de la build oficial adecuada (CUDA/ROCm(HIP)/Metal/Vulkan/CPU),
//! descarga de GGUF y arranque de `llama-server` (API compatible con OpenAI).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use astrodev_hardware::{Backend, HardwareInfo};

use crate::runtime::{self, RuntimeStatus};
use crate::{download, extract, paths, InstallError, Reporter, Result};

pub const RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";
pub const DEFAULT_PORT: u16 = 8080;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

/// Elige los assets de la release para la plataforma y el backend.
/// Devuelve (asset principal, assets extra como el runtime de CUDA en Windows).
pub fn select_assets<'a>(
    assets: &'a [Asset],
    os: &str,
    arch: &str,
    backend: Backend,
) -> Option<(&'a Asset, Vec<&'a Asset>)> {
    let zip = |a: &&Asset| a.name.ends_with(".zip") || a.name.ends_with(".tar.gz");
    let has = |a: &Asset, parts: &[&str]| parts.iter().all(|p| a.name.contains(p));
    let x64 = arch == "x86_64";
    let find = |parts: &[&str]| assets.iter().filter(zip).find(|a| has(a, parts) && a.name.starts_with("llama-"));

    let main = match os {
        "macos" => find(&["bin-macos", if x64 { "x64" } else { "arm64" }]),
        "windows" => {
            let arch_s = if x64 { "x64" } else { "arm64" };
            match backend {
                Backend::Cuda => {
                    // La versión de CUDA más alta disponible.
                    let mut c: Vec<&Asset> = assets
                        .iter()
                        .filter(zip)
                        .filter(|a| a.name.starts_with("llama-") && has(a, &["bin-win-cuda", arch_s]))
                        .collect();
                    c.sort_by(|a, b| b.name.cmp(&a.name));
                    c.first().copied()
                }
                Backend::Rocm => find(&["bin-win-hip", arch_s]).or_else(|| find(&["bin-win-vulkan", arch_s])),
                Backend::Vulkan => find(&["bin-win-vulkan", arch_s]),
                _ => None,
            }
            .or_else(|| find(&["bin-win-cpu", arch_s]))
        }
        _ => {
            // Linux: no hay build oficial de CUDA/ROCm; Vulkan acelera NVIDIA, AMD e Intel.
            let arch_s = if x64 { "x64" } else { "arm64" };
            match backend {
                Backend::Cpu => None,
                _ => find(&["bin-ubuntu-vulkan", arch_s]),
            }
            .or_else(|| {
                assets.iter().filter(zip).find(|a| {
                    a.name.starts_with("llama-") && has(a, &["bin-ubuntu", arch_s]) && !a.name.contains("vulkan")
                })
            })
        }
    }?;

    let mut extra = Vec::new();
    if os == "windows" && main.name.contains("cuda") {
        // cudart-llama-bin-win-cuda-12.4-x64.zip con la misma versión de CUDA.
        let ver = main.name.split("cuda-").nth(1).and_then(|s| s.split('-').next()).unwrap_or("");
        if let Some(rt) =
            assets.iter().find(|a| a.name.starts_with("cudart-") && a.name.contains(&format!("cuda-{ver}")))
        {
            extra.push(rt);
        }
    }
    Some((main, extra))
}

pub async fn latest_release() -> Result<Release> {
    let r = crate::http()
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| InstallError::Network(e.to_string()))?;
    if !r.status().is_success() {
        return Err(InstallError::Http { status: r.status().as_u16(), url: RELEASES_API.into() });
    }
    r.json().await.map_err(|e| InstallError::Network(e.to_string()))
}

/// Instala la build adecuada de llama.cpp en el directorio de AstroDev.
pub async fn install(hw: &HardwareInfo, rep: &Reporter, cancel: &CancellationToken) -> Result<PathBuf> {
    if let Some(b) = runtime::llamacpp_managed_bin() {
        rep.log(format!("llama.cpp ya instalado en {}", b.display()));
        return Ok(b);
    }
    rep.step("Buscando la última versión de llama.cpp");
    let rel = latest_release().await?;
    let (main, extra) = select_assets(&rel.assets, &hw.os, &hw.arch, hw.backend()).ok_or_else(|| {
        InstallError::Other(format!("no hay build de llama.cpp {} para {}/{}", rel.tag_name, hw.os, hw.arch))
    })?;
    rep.step(format!("Instalando llama.cpp {} ({})", rel.tag_name, main.name));
    let dest = runtime::llamacpp_managed_dir();
    for a in std::iter::once(main).chain(extra) {
        let tmp = paths::runtimes_dir().join(".downloads").join(&a.name);
        download::download(&a.browser_download_url, &tmp, None, &a.name, rep, cancel).await?;
        let (t2, d2) = (tmp.clone(), dest.clone());
        tokio::task::spawn_blocking(move || extract::extract(&t2, &d2))
            .await
            .map_err(|e| InstallError::Other(e.to_string()))??;
        let _ = std::fs::remove_file(&tmp);
    }
    let bin = runtime::llamacpp_managed_bin()
        .ok_or_else(|| InstallError::Other("no se encontró llama-server tras extraer".into()))?;
    extract::make_executable(&bin)?;
    Ok(bin)
}

/// Descarga un GGUF al directorio de modelos.
pub async fn download_gguf(
    url: &str,
    sha256: Option<&str>,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let file = url.rsplit('/').next().unwrap_or("modelo.gguf");
    let dest = paths::models_dir().join(file);
    download::download(url, &dest, sha256, file, rep, cancel).await
}

/// Argumentos de `llama-server`.
pub fn server_args(model: &Path, ctx: u32, gpu_layers: u32, port: u16) -> Vec<String> {
    vec![
        "-m".into(),
        model.display().to_string(),
        "-c".into(),
        ctx.to_string(),
        "-ngl".into(),
        gpu_layers.to_string(),
        "--port".into(),
        port.to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        // Plantillas Jinja: necesarias para el tool calling nativo.
        "--jinja".into(),
    ]
}

/// Arranca `llama-server` con un modelo y espera a que esté listo.
pub async fn start_server(
    bin: &Path,
    model: &Path,
    ctx: u32,
    gpu_layers: u32,
    rep: &Reporter,
) -> Result<RuntimeStatus> {
    let base = format!("http://127.0.0.1:{DEFAULT_PORT}");
    if runtime::llamacpp_status(&base).await.running {
        rep.log("Ya hay un llama-server en marcha en el puerto 8080; lo reutilizo");
        return Ok(runtime::llamacpp_status(&base).await);
    }
    rep.step(format!("Arrancando llama-server (contexto {ctx}, {gpu_layers} capas en GPU)"));
    runtime::spawn_background(bin, &server_args(model, ctx, gpu_layers, DEFAULT_PORT), "llama-server.log", &[])?;
    if !runtime::wait_until_up(&format!("{base}/health"), Duration::from_secs(180)).await {
        return Err(InstallError::Other(format!(
            "llama-server no arrancó; revisa {}",
            paths::logs_dir().join("llama-server.log").display()
        )));
    }
    Ok(runtime::llamacpp_status(&base).await)
}

pub fn uninstall_managed() -> Result<String> {
    let dir = runtime::llamacpp_managed_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
        Ok(format!("Eliminado {}", dir.display()))
    } else {
        Ok("llama.cpp no lo instaló AstroDev; nada que borrar.".into())
    }
}

/// Lista GGUF descargados.
pub fn list_models() -> Vec<(String, u64)> {
    let Ok(rd) = std::fs::read_dir(paths::models_dir()) else { return Vec::new() };
    let mut v: Vec<(String, u64)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "gguf"))
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    v.sort();
    v
}

pub fn delete_model(file: &str) -> Result<()> {
    let name = Path::new(file).file_name().ok_or_else(|| InstallError::Other("nombre inválido".into()))?;
    let p = paths::models_dir().join(name);
    if !p.exists() {
        return Err(InstallError::Other(format!("no existe {}", p.display())));
    }
    std::fs::remove_file(&p)?;
    let _ = std::fs::remove_file(download::part_path(&p));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> Vec<Asset> {
        [
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.1-x64.zip",
            "llama-b7000-bin-macos-arm64.zip",
            "llama-b7000-bin-macos-x64.zip",
            "llama-b7000-bin-ubuntu-vulkan-x64.zip",
            "llama-b7000-bin-ubuntu-x64.zip",
            "llama-b7000-bin-ubuntu-arm64.zip",
            "llama-b7000-bin-win-cpu-arm64.zip",
            "llama-b7000-bin-win-cpu-x64.zip",
            "llama-b7000-bin-win-cuda-12.4-x64.zip",
            "llama-b7000-bin-win-cuda-13.1-x64.zip",
            "llama-b7000-bin-win-hip-radeon-x64.zip",
            "llama-b7000-bin-win-vulkan-x64.zip",
            "llama-b7000-xcframework.zip",
        ]
        .iter()
        .map(|n| Asset { name: n.to_string(), browser_download_url: format!("https://x/{n}"), size: 1 })
        .collect()
    }

    fn pick(os: &str, arch: &str, b: Backend) -> (String, Vec<String>) {
        let a = assets();
        let (m, e) = select_assets(&a, os, arch, b).unwrap();
        (m.name.clone(), e.iter().map(|x| x.name.clone()).collect())
    }

    #[test]
    fn selects_correct_builds() {
        assert_eq!(pick("macos", "aarch64", Backend::Metal).0, "llama-b7000-bin-macos-arm64.zip");
        assert_eq!(pick("linux", "x86_64", Backend::Cuda).0, "llama-b7000-bin-ubuntu-vulkan-x64.zip");
        assert_eq!(pick("linux", "x86_64", Backend::Cpu).0, "llama-b7000-bin-ubuntu-x64.zip");
        assert_eq!(pick("linux", "aarch64", Backend::Cpu).0, "llama-b7000-bin-ubuntu-arm64.zip");
        let (m, e) = pick("windows", "x86_64", Backend::Cuda);
        assert_eq!(m, "llama-b7000-bin-win-cuda-13.1-x64.zip");
        assert_eq!(e, vec!["cudart-llama-bin-win-cuda-13.1-x64.zip"]);
        assert_eq!(pick("windows", "x86_64", Backend::Rocm).0, "llama-b7000-bin-win-hip-radeon-x64.zip");
        assert_eq!(pick("windows", "x86_64", Backend::Vulkan).0, "llama-b7000-bin-win-vulkan-x64.zip");
        assert_eq!(pick("windows", "x86_64", Backend::Cpu).0, "llama-b7000-bin-win-cpu-x64.zip");
        assert_eq!(pick("windows", "aarch64", Backend::Cpu).0, "llama-b7000-bin-win-cpu-arm64.zip");
    }

    #[test]
    fn args_enable_jinja_and_offload() {
        let a = server_args(Path::new("/m/q.gguf"), 16384, 28, 8080);
        assert!(a.windows(2).any(|w| w == ["-ngl", "28"]));
        assert!(a.windows(2).any(|w| w == ["-c", "16384"]));
        assert!(a.contains(&"--jinja".to_string()));
    }
}
