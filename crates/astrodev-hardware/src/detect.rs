//! Detección de hardware multiplataforma.
//!
//! CPU/RAM/disco con `sysinfo`. GPU:
//! - NVIDIA: `nvidia-smi` (Linux y Windows).
//! - AMD/Intel en Linux: `/sys/class/drm/card*/device` (`mem_info_vram_total`).
//! - Apple Silicon: memoria unificada (Metal puede usar ~70-75 % de la RAM).
//! - Windows sin NVIDIA: registro vía PowerShell (`HardwareInformation.qwMemorySize`).

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
}

/// Backend de aceleración recomendado para llama.cpp / Ollama.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Cuda,
    Rocm,
    Metal,
    Vulkan,
    Cpu,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::Cuda => "CUDA",
            Backend::Rocm => "ROCm",
            Backend::Metal => "Metal",
            Backend::Vulkan => "Vulkan",
            Backend::Cpu => "CPU",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub name: String,
    pub vram_total: u64,
    pub vram_free: Option<u64>,
    /// Memoria compartida con la CPU (Apple Silicon, iGPU).
    pub unified: bool,
    pub driver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuInfo {
    pub brand: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub os: String,
    pub arch: String,
    pub cpu: CpuInfo,
    pub ram_total: u64,
    pub ram_available: u64,
    pub gpus: Vec<GpuInfo>,
    /// Espacio libre donde se guardan los modelos.
    pub disk_free: u64,
}

impl HardwareInfo {
    /// GPU dedicada principal (la de más VRAM, no unificada) si la hay.
    pub fn best_gpu(&self) -> Option<&GpuInfo> {
        self.gpus.iter().filter(|g| !g.unified || g.vendor == GpuVendor::Apple).max_by_key(|g| g.vram_total)
    }

    pub fn is_apple_silicon(&self) -> bool {
        self.gpus.iter().any(|g| g.vendor == GpuVendor::Apple)
    }

    pub fn total_vram(&self) -> u64 {
        self.gpus.iter().filter(|g| !g.unified).map(|g| g.vram_total).sum()
    }

    pub fn backend(&self) -> Backend {
        match self.best_gpu().map(|g| g.vendor) {
            Some(GpuVendor::Nvidia) => Backend::Cuda,
            Some(GpuVendor::Amd) => Backend::Rocm,
            Some(GpuVendor::Apple) => Backend::Metal,
            Some(GpuVendor::Intel) | Some(GpuVendor::Other) => Backend::Vulkan,
            None => Backend::Cpu,
        }
    }
}

/// Variable para sustituir la detección por un JSON (tests, demos, informes de errores).
pub const OVERRIDE_ENV: &str = "ASTRODEV_HW_OVERRIDE";

/// Detecta el hardware. `models_dir` se usa para medir el espacio libre.
/// Si `ASTRODEV_HW_OVERRIDE` apunta a un JSON con un `HardwareInfo`, se usa ese.
pub fn detect(models_dir: &Path) -> HardwareInfo {
    if let Ok(path) = std::env::var(OVERRIDE_ENV) {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(hw) => return hw,
            Err(e) => tracing::warn!("{OVERRIDE_ENV}={path} no es válido: {e}"),
        }
    }
    detect_real(models_dir)
}

fn detect_real(models_dir: &Path) -> HardwareInfo {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let brand = sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
    let logical = sys.cpus().len().max(1);
    let physical = sys.physical_core_count().unwrap_or(logical);

    let cpu = CpuInfo {
        brand: if brand.is_empty() { cpu_brand_fallback() } else { brand },
        physical_cores: physical,
        logical_cores: logical,
        avx2: has_avx2(),
        avx512: has_avx512(),
        neon: cfg!(target_arch = "aarch64"),
    };

    let disk_free = disk_free(models_dir);

    let ram_total = sys.total_memory();
    let mut gpus = detect_gpus(ram_total);
    gpus.sort_by_key(|g| std::cmp::Reverse(g.vram_total));

    HardwareInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu,
        ram_total,
        ram_available: sys.available_memory(),
        gpus,
        disk_free,
    }
}

/// Espacio libre (bytes) en el disco que contiene `path` (0 si no se sabe).
pub fn disk_free(path: &Path) -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let probe = existing_ancestor(path);
    disks
        .list()
        .iter()
        .filter(|d| probe.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
        .unwrap_or(0)
}

fn existing_ancestor(p: &Path) -> std::path::PathBuf {
    let mut cur = p.to_path_buf();
    while !cur.exists() {
        if !cur.pop() {
            break;
        }
    }
    std::fs::canonicalize(&cur).unwrap_or(cur)
}

fn cpu_brand_fallback() -> String {
    format!("CPU {}", std::env::consts::ARCH)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_avx2() -> bool {
    false
}
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_avx512() -> bool {
    std::arch::is_x86_feature_detected!("avx512f")
}
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_avx512() -> bool {
    false
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn detect_gpus(ram_total: u64) -> Vec<GpuInfo> {
    let mut gpus = Vec::new();
    if let Some(out) = run(
        "nvidia-smi",
        &["--query-gpu=name,memory.total,memory.free,driver_version", "--format=csv,noheader,nounits"],
    ) {
        gpus.extend(parse_nvidia_smi(&out));
    }
    if cfg!(target_os = "macos") {
        if let Some(g) = detect_apple(ram_total) {
            gpus.push(g);
        }
    }
    if cfg!(target_os = "linux") {
        let have_nvidia = !gpus.is_empty();
        for g in detect_linux_drm(Path::new("/sys/class/drm"), ram_total) {
            if have_nvidia && g.vendor == GpuVendor::Nvidia {
                continue;
            }
            gpus.push(g);
        }
    }
    if cfg!(target_os = "windows") && gpus.iter().all(|g| g.vendor != GpuVendor::Nvidia) {
        let script = r#"Get-ItemProperty -Path 'HKLM:\SYSTEM\ControlSet001\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' -ErrorAction SilentlyContinue | Select-Object DriverDesc,'HardwareInformation.qwMemorySize' | ConvertTo-Json"#;
        if let Some(out) = run("powershell", &["-NoProfile", "-Command", script]) {
            gpus.extend(parse_windows_registry(&out));
        }
    }
    gpus
}

/// `nvidia-smi --query-gpu=name,memory.total,memory.free,driver_version --format=csv,noheader,nounits`
pub fn parse_nvidia_smi(out: &str) -> Vec<GpuInfo> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            if f.len() < 3 {
                return None;
            }
            let mib = |s: &str| s.parse::<f64>().ok().map(|v| (v * 1024.0 * 1024.0) as u64);
            Some(GpuInfo {
                vendor: GpuVendor::Nvidia,
                name: f[0].to_string(),
                vram_total: mib(f[1])?,
                vram_free: mib(f[2]),
                unified: false,
                driver: f.get(3).map(|s| s.to_string()),
            })
        })
        .collect()
}

fn detect_apple(ram_total: u64) -> Option<GpuInfo> {
    let brand = run("sysctl", &["-n", "machdep.cpu.brand_string"])?;
    if !brand.contains("Apple") {
        return None;
    }
    Some(apple_gpu(brand.trim(), ram_total))
}

/// Apple Silicon: Metal puede reservar ~2/3 de la RAM (75 % a partir de 36 GB).
pub fn apple_gpu(brand: &str, ram_total: u64) -> GpuInfo {
    let frac = if ram_total as f64 >= 36e9 { 0.75 } else { 0.67 };
    GpuInfo {
        vendor: GpuVendor::Apple,
        name: format!("{brand} (GPU integrada)"),
        vram_total: (ram_total as f64 * frac) as u64,
        vram_free: None,
        unified: true,
        driver: Some("Metal".into()),
    }
}

/// Recorre `/sys/class/drm/card*/device`.
pub fn detect_linux_drm(root: &Path, ram_total: u64) -> Vec<GpuInfo> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    let mut names: Vec<_> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for n in names {
        // Sólo cardN (no cardN-HDMI-A-1).
        if !n.starts_with("card") || n[4..].contains('-') {
            continue;
        }
        let dev = root.join(&n).join("device");
        let read = |f: &str| std::fs::read_to_string(dev.join(f)).ok().map(|s| s.trim().to_string());
        let Some(vendor_id) = read("vendor") else { continue };
        let vendor = match vendor_id.as_str() {
            "0x1002" => GpuVendor::Amd,
            "0x8086" => GpuVendor::Intel,
            "0x10de" => GpuVendor::Nvidia,
            _ => GpuVendor::Other,
        };
        let vram_total = read("mem_info_vram_total").and_then(|s| s.parse::<u64>().ok());
        let vram_used = read("mem_info_vram_used").and_then(|s| s.parse::<u64>().ok());
        let device = read("device").unwrap_or_default();
        let name = read("product_name").filter(|s| !s.is_empty()).unwrap_or_else(|| {
            let v = match vendor {
                GpuVendor::Amd => "AMD Radeon",
                GpuVendor::Intel => "Intel Graphics",
                GpuVendor::Nvidia => "NVIDIA",
                _ => "GPU",
            };
            format!("{v} ({device})")
        });
        // Una iGPU AMD/Intel sin VRAM dedicada significativa comparte la RAM.
        let (vram, unified) = match vram_total {
            Some(v) if v >= 2_000_000_000 => (v, false),
            _ => ((ram_total as f64 * 0.5) as u64, true),
        };
        out.push(GpuInfo {
            vendor,
            name,
            vram_total: vram,
            vram_free: vram_total.zip(vram_used).map(|(t, u)| t.saturating_sub(u)),
            unified,
            driver: None,
        });
    }
    out
}

/// Salida JSON de PowerShell con `DriverDesc` y `HardwareInformation.qwMemorySize`.
pub fn parse_windows_registry(out: &str) -> Vec<GpuInfo> {
    let v: serde_json::Value = match serde_json::from_str(out.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let items = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|i| {
            let name = i.get("DriverDesc")?.as_str()?.to_string();
            let mem = i.get("HardwareInformation.qwMemorySize").and_then(|m| m.as_u64()).unwrap_or(0);
            let lower = name.to_lowercase();
            let vendor = if lower.contains("nvidia") {
                GpuVendor::Nvidia
            } else if lower.contains("amd") || lower.contains("radeon") {
                GpuVendor::Amd
            } else if lower.contains("intel") {
                GpuVendor::Intel
            } else {
                GpuVendor::Other
            };
            if lower.contains("basic display") || lower.contains("remote") {
                return None;
            }
            Some(GpuInfo { vendor, name, vram_total: mem, vram_free: None, unified: mem < 2_000_000_000, driver: None })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_parsing() {
        let g = parse_nvidia_smi(
            "NVIDIA GeForce RTX 4070, 12282, 11530, 550.54.14\nNVIDIA GeForce RTX 3090, 24576, 24000, 550.54.14\n",
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 4070");
        assert_eq!(g[0].vram_total, 12282 * 1024 * 1024);
        assert_eq!(g[1].driver.as_deref(), Some("550.54.14"));
        assert!(parse_nvidia_smi("basura").is_empty());
    }

    #[test]
    fn linux_drm_amd_and_intel() {
        let d = tempfile::tempdir().unwrap();
        let card = |n: &str, vendor: &str, vram: Option<&str>| {
            let dev = d.path().join(n).join("device");
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(dev.join("vendor"), vendor).unwrap();
            std::fs::write(dev.join("device"), "0x744c").unwrap();
            if let Some(v) = vram {
                std::fs::write(dev.join("mem_info_vram_total"), v).unwrap();
                std::fs::write(dev.join("mem_info_vram_used"), "1000000000").unwrap();
            }
        };
        card("card0", "0x8086\n", None);
        card("card1", "0x1002\n", Some("25753026560\n"));
        std::fs::create_dir_all(d.path().join("card1-DP-1")).unwrap();
        let g = detect_linux_drm(d.path(), 32_000_000_000);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].vendor, GpuVendor::Intel);
        assert!(g[0].unified);
        assert_eq!(g[1].vendor, GpuVendor::Amd);
        assert_eq!(g[1].vram_total, 25753026560);
        assert_eq!(g[1].vram_free, Some(24753026560));
        assert!(!g[1].unified);
    }

    #[test]
    fn windows_registry_parsing() {
        let out = r#"[{"DriverDesc":"AMD Radeon RX 7800 XT","HardwareInformation.qwMemorySize":17163091968},
                      {"DriverDesc":"Microsoft Basic Display Adapter","HardwareInformation.qwMemorySize":0}]"#;
        let g = parse_windows_registry(out);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].vendor, GpuVendor::Amd);
        assert!(!g[0].unified);
        let one = r#"{"DriverDesc":"Intel(R) UHD Graphics","HardwareInformation.qwMemorySize":1073741824}"#;
        assert!(parse_windows_registry(one)[0].unified);
    }

    #[test]
    fn apple_unified_memory() {
        let g = apple_gpu("Apple M3 Pro", 36_000_000_000);
        assert_eq!(g.vendor, GpuVendor::Apple);
        assert_eq!(g.vram_total, 27_000_000_000);
        assert!(g.unified);
    }

    #[test]
    fn backend_selection() {
        let mut hw = HardwareInfo {
            os: "linux".into(),
            arch: "x86_64".into(),
            cpu: CpuInfo {
                brand: "x".into(),
                physical_cores: 8,
                logical_cores: 16,
                avx2: true,
                avx512: false,
                neon: false,
            },
            ram_total: 32_000_000_000,
            ram_available: 28_000_000_000,
            gpus: vec![],
            disk_free: 0,
        };
        assert_eq!(hw.backend(), Backend::Cpu);
        hw.gpus = parse_nvidia_smi("RTX 4090, 24564, 24000, 1");
        assert_eq!(hw.backend(), Backend::Cuda);
    }

    #[test]
    fn detect_runs_here() {
        let hw = detect(Path::new("."));
        assert!(hw.ram_total > 0);
        assert!(hw.cpu.logical_cores >= 1);
    }
}
