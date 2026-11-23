//! Recomendador: qué modelos del catálogo caben en este equipo y a qué velocidad.

use serde::{Deserialize, Serialize};

use crate::bandwidth::{cpu_bandwidth, gpu_bandwidth, gpu_bandwidth_fallback};
use crate::catalog::{Catalog, CatalogModel, QuantVariant};
use crate::detect::HardwareInfo;
use crate::memory::{bits_per_weight, kv_cache_bytes, quality_factor, MemoryEstimate};

/// Fracción del ancho de banda teórico que se aprovecha en la práctica.
const EFFICIENCY: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fit {
    Recommended,
    Possible,
    TooBig,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Recommended => "recomendado",
            Fit::Possible => "posible",
            Fit::TooBig => "demasiado grande",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Placement {
    /// Todo en GPU (o memoria unificada).
    FullGpu,
    /// Parte de las capas en GPU y el resto en CPU.
    Partial {
        gpu_layers: u32,
        total_layers: u32,
    },
    Cpu,
    /// No cabe.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RecommendOptions {
    /// Contexto deseado en tokens.
    pub ctx: u32,
    /// Bytes por valor de la KV cache (2.0 = f16).
    pub kv_bytes: f64,
}

impl Default for RecommendOptions {
    fn default() -> Self {
        Self { ctx: 8192, kv_bytes: 2.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub model_id: String,
    pub model_name: String,
    pub quant: String,
    pub ollama_pull: Option<String>,
    pub gguf_url: Option<String>,
    pub download_bytes: u64,
    pub ctx: u32,
    pub memory: MemoryEstimate,
    pub placement: Placement,
    pub fit: Fit,
    pub tokens_per_second: f64,
    pub supports_tools: bool,
    pub code_score: u32,
    pub score: f64,
    pub reasons: Vec<String>,
}

/// Memoria utilizable del equipo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub gpu: u64,
    pub ram: u64,
    pub gpu_bw: f64,
    pub cpu_bw: f64,
    pub unified: bool,
}

pub fn budget(hw: &HardwareInfo) -> Budget {
    let cpu_bw = cpu_bandwidth(hw.cpu.avx512, hw.cpu.physical_cores);
    match hw.best_gpu() {
        Some(g) if g.unified => Budget {
            gpu: g.vram_total.saturating_sub(1_000_000_000),
            ram: 0,
            gpu_bw: gpu_bandwidth(&g.name).unwrap_or(100.0),
            cpu_bw,
            unified: true,
        },
        Some(g) => Budget {
            gpu: ((hw.total_vram() as f64) * 0.92) as u64 - 300_000_000u64.min(hw.total_vram()),
            ram: (hw.ram_total as f64 * 0.6) as u64,
            gpu_bw: gpu_bandwidth(&g.name).unwrap_or_else(|| gpu_bandwidth_fallback(g.vram_total)),
            cpu_bw,
            unified: false,
        },
        None => Budget { gpu: 0, ram: (hw.ram_total as f64 * 0.7) as u64, gpu_bw: 0.0, cpu_bw, unified: false },
    }
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

/// Evalúa una cuantización concreta.
fn evaluate(
    m: &CatalogModel,
    q: &QuantVariant,
    ctx: u32,
    kv_bytes: f64,
    b: &Budget,
) -> (MemoryEstimate, Placement, f64) {
    let kv = kv_cache_bytes(m.layers, m.kv_heads, m.head_dim, ctx, kv_bytes);
    let mem = MemoryEstimate::new(q.size_bytes, kv);
    let read_per_token = q.size_bytes as f64 * m.active_fraction();
    if b.gpu > 0 && mem.total <= b.gpu {
        let tps = EFFICIENCY * b.gpu_bw * 1e9 / read_per_token;
        return (mem, Placement::FullGpu, tps);
    }
    if b.gpu > 0 && !b.unified && mem.total <= b.gpu + b.ram {
        let gpu_for_weights = b.gpu.saturating_sub(kv + mem.overhead) as f64;
        let frac = (gpu_for_weights / q.size_bytes as f64).clamp(0.0, 1.0);
        let t = read_per_token * frac / (b.gpu_bw * 1e9) + read_per_token * (1.0 - frac) / (b.cpu_bw * 1e9);
        let layers = (m.layers as f64 * frac).floor() as u32;
        return (mem, Placement::Partial { gpu_layers: layers, total_layers: m.layers }, EFFICIENCY / t);
    }
    if b.gpu == 0 && mem.total <= b.ram {
        return (mem, Placement::Cpu, EFFICIENCY * b.cpu_bw * 1e9 / read_per_token);
    }
    (mem, Placement::None, 0.0)
}

fn recommend_model(m: &CatalogModel, hw: &HardwareInfo, b: &Budget, opts: &RecommendOptions) -> Recommendation {
    let ctx = opts.ctx.min(m.context_max);
    let mut quants: Vec<&QuantVariant> = m.quants.iter().collect();
    quants.sort_by(|a, c| bits_per_weight(&c.name).total_cmp(&bits_per_weight(&a.name)));

    let evals: Vec<(&QuantVariant, MemoryEstimate, Placement, f64)> = quants
        .iter()
        .map(|q| {
            let (mem, p, tps) = evaluate(m, q, ctx, opts.kv_bytes, b);
            (*q, mem, p, tps)
        })
        .collect();

    let fast = |p: &Placement, tps: f64| matches!(p, Placement::FullGpu | Placement::Cpu) && tps >= 10.0;
    // 1) la mejor calidad que vaya rápida; 2) la que más rinda entre las que caben; 3) la más pequeña.
    let chosen = evals
        .iter()
        .find(|(_, _, p, tps)| fast(p, *tps))
        .or_else(|| {
            evals
                .iter()
                .filter(|(_, _, p, _)| *p != Placement::None)
                .max_by(|a, c| (a.3 * quality_factor(&a.0.name)).total_cmp(&(c.3 * quality_factor(&c.0.name))))
        })
        .or_else(|| evals.last())
        .expect("al menos una cuantización");
    let (q, mem, placement, tps) = (chosen.0, chosen.1, chosen.2, chosen.3);

    let mut reasons = Vec::new();
    let disk_ok = hw.disk_free == 0 || q.size_bytes + 1_000_000_000 < hw.disk_free;
    let mut fit = match placement {
        Placement::None => Fit::TooBig,
        _ if tps < 1.5 => Fit::TooBig,
        Placement::FullGpu if tps >= 10.0 => Fit::Recommended,
        Placement::Cpu if tps >= 8.0 => Fit::Recommended,
        _ => Fit::Possible,
    };
    match placement {
        Placement::FullGpu if b.unified => {
            reasons.push(format!("cabe en memoria unificada ({} de {})", gb(mem.total), gb(b.gpu)))
        }
        Placement::FullGpu => reasons.push(format!("cabe entero en VRAM ({} de {})", gb(mem.total), gb(b.gpu))),
        Placement::Partial { gpu_layers, total_layers } => reasons.push(format!(
            "necesita {}: {gpu_layers}/{total_layers} capas en GPU y el resto en RAM (más lento)",
            gb(mem.total)
        )),
        Placement::Cpu => reasons.push(format!("sólo CPU: usa {} de RAM", gb(mem.total))),
        Placement::None => {
            reasons.push(format!("necesita {} y tienes {} utilizables", gb(mem.total), gb(b.gpu + b.ram)))
        }
    }
    if placement != Placement::None {
        reasons.push(format!("~{tps:.0} tok/s estimados"));
        if tps < 1.5 {
            reasons.push("demasiado lento para trabajar con él".into());
        }
    }
    if let Some(a) = m.active_params_b {
        reasons.push(format!("MoE: sólo {a:.1}B parámetros activos por token"));
    }
    reasons.push(if m.supports_tools {
        "tool calling nativo".into()
    } else {
        "sin tools nativas (modo por prompt)".into()
    });
    if ctx < opts.ctx {
        reasons.push(format!("contexto máximo del modelo: {ctx}"));
    }
    if !disk_ok {
        fit = Fit::TooBig;
        reasons.push(format!("no hay espacio en disco: necesita {} y quedan {}", gb(q.size_bytes), gb(hw.disk_free)));
    }

    let score = m.code_score as f64 * quality_factor(&q.name) * if m.supports_tools { 1.0 } else { 0.88 }
        + tps.min(60.0) / 60.0 * 8.0;

    Recommendation {
        model_id: m.id.clone(),
        model_name: m.name.clone(),
        quant: q.name.clone(),
        ollama_pull: q.ollama_pull_name(),
        gguf_url: q.gguf.as_ref().map(|g| g.url()),
        download_bytes: q.size_bytes,
        ctx,
        memory: mem,
        placement,
        fit,
        tokens_per_second: tps,
        supports_tools: m.supports_tools,
        code_score: m.code_score,
        score,
        reasons,
    }
}

/// Ranking de todo el catálogo: recomendados primero, luego posibles y demasiado grandes;
/// dentro de cada grupo, por puntuación.
pub fn recommend(hw: &HardwareInfo, catalog: &Catalog, opts: &RecommendOptions) -> Vec<Recommendation> {
    let b = budget(hw);
    let mut out: Vec<Recommendation> = catalog.models.iter().map(|m| recommend_model(m, hw, &b, opts)).collect();
    out.sort_by(|a, c| a.fit.cmp(&c.fit).then(c.score.total_cmp(&a.score)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{apple_gpu, parse_nvidia_smi, CpuInfo};

    fn hw(ram_gb: f64, gpus: Vec<crate::detect::GpuInfo>) -> HardwareInfo {
        HardwareInfo {
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
            ram_total: (ram_gb * 1e9) as u64,
            ram_available: (ram_gb * 0.8e9) as u64,
            gpus,
            disk_free: 500_000_000_000,
        }
    }

    fn find<'a>(r: &'a [Recommendation], id: &str) -> &'a Recommendation {
        r.iter().find(|x| x.model_id == id).unwrap()
    }

    #[test]
    fn rtx_4070_12gb() {
        let h = hw(32.0, parse_nvidia_smi("NVIDIA GeForce RTX 4070, 12282, 11800, 550"));
        let r = recommend(&h, &Catalog::embedded(), &RecommendOptions::default());
        let q7 = find(&r, "qwen2.5-coder-7b");
        assert_eq!(q7.fit, Fit::Recommended);
        assert_eq!(q7.placement, Placement::FullGpu);
        // con 12 GB cabe en Q8_0 (8.1 GB + KV)
        assert_eq!(q7.quant, "Q8_0");
        assert!(q7.tokens_per_second > 30.0, "{}", q7.tokens_per_second);

        let q14 = find(&r, "qwen2.5-coder-14b");
        assert_eq!(q14.fit, Fit::Recommended);
        assert_eq!(q14.quant, "Q4_K_M");

        let q32 = find(&r, "qwen2.5-coder-32b");
        assert_eq!(q32.fit, Fit::Possible);
        assert!(matches!(q32.placement, Placement::Partial { .. }));

        // el primero es un recomendado
        assert_eq!(r[0].fit, Fit::Recommended);
        // orden por grupos
        let fits: Vec<Fit> = r.iter().map(|x| x.fit).collect();
        let mut sorted = fits.clone();
        sorted.sort();
        assert_eq!(fits, sorted);
    }

    #[test]
    fn cpu_only_laptop_8gb() {
        let h = hw(8.0, vec![]);
        let r = recommend(&h, &Catalog::embedded(), &RecommendOptions::default());
        assert_eq!(find(&r, "qwen2.5-coder-1.5b").placement, Placement::Cpu);
        assert_eq!(find(&r, "qwen2.5-coder-1.5b").fit, Fit::Recommended);
        assert_eq!(find(&r, "qwen2.5-coder-32b").fit, Fit::TooBig);
        assert!(find(&r, "qwen2.5-coder-32b").reasons[0].contains("necesita"));
    }

    #[test]
    fn apple_m3_max_64gb_runs_32b() {
        let h = HardwareInfo { gpus: vec![apple_gpu("Apple M3 Max", 64_000_000_000)], ..hw(64.0, vec![]) };
        let r = recommend(&h, &Catalog::embedded(), &RecommendOptions::default());
        let q32 = find(&r, "qwen2.5-coder-32b");
        assert_eq!(q32.placement, Placement::FullGpu);
        assert_eq!(q32.fit, Fit::Recommended);
        assert!(q32.reasons[0].contains("memoria unificada"));
    }

    #[test]
    fn moe_is_faster_than_dense() {
        let h = hw(64.0, parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564, 24000, 550"));
        let r = recommend(&h, &Catalog::embedded(), &RecommendOptions::default());
        let moe = find(&r, "qwen3-coder-30b-a3b");
        let dense = find(&r, "qwen2.5-coder-32b");
        assert!(moe.tokens_per_second > dense.tokens_per_second * 3.0);
    }

    #[test]
    fn larger_context_needs_more_memory() {
        let h = hw(32.0, parse_nvidia_smi("NVIDIA GeForce RTX 4060, 8188, 8000, 550"));
        let c = Catalog::embedded();
        let small = recommend(&h, &c, &RecommendOptions { ctx: 4096, ..Default::default() });
        let big = recommend(&h, &c, &RecommendOptions { ctx: 32768, ..Default::default() });
        let a = find(&small, "qwen2.5-coder-7b");
        let b = find(&big, "qwen2.5-coder-7b");
        assert!(b.memory.kv_cache > a.memory.kv_cache * 7);
    }

    #[test]
    fn no_disk_space_marks_too_big() {
        let mut h = hw(32.0, parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564, 24000, 550"));
        h.disk_free = 3_000_000_000;
        let r = recommend(&h, &Catalog::embedded(), &RecommendOptions::default());
        let m = find(&r, "qwen2.5-coder-14b");
        assert_eq!(m.fit, Fit::TooBig);
        assert!(m.reasons.iter().any(|x| x.contains("espacio en disco")));
        assert_eq!(find(&r, "qwen2.5-coder-0.5b").fit, Fit::Recommended);
    }
}

/// Estimación para un modelo del que sólo se conoce el tamaño (p. ej. instalado en Ollama
/// pero ausente del catálogo): se suponen ~0,6 GB de KV cache con el contexto por defecto.
pub fn estimate_by_size(hw: &HardwareInfo, size_bytes: u64) -> (Fit, String) {
    let b = budget(hw);
    let mem = MemoryEstimate::new(size_bytes, 600_000_000);
    let gbs = |x: u64| format!("{:.1} GB", x as f64 / 1e9);
    if b.gpu > 0 && mem.total <= b.gpu {
        let tps = EFFICIENCY * b.gpu_bw * 1e9 / size_bytes.max(1) as f64;
        let where_ = if b.unified { "memoria unificada" } else { "VRAM" };
        (if tps >= 10.0 { Fit::Recommended } else { Fit::Possible }, format!("cabe en {where_} · ~{tps:.0} tok/s"))
    } else if b.gpu > 0 && !b.unified && mem.total <= b.gpu + b.ram {
        (Fit::Possible, format!("{} · repartido GPU/CPU (más lento)", gbs(mem.total)))
    } else if b.gpu == 0 && mem.total <= b.ram {
        let tps = EFFICIENCY * b.cpu_bw * 1e9 / size_bytes.max(1) as f64;
        (if tps >= 8.0 { Fit::Recommended } else { Fit::Possible }, format!("CPU · ~{tps:.0} tok/s"))
    } else {
        (Fit::TooBig, format!("necesita ~{} y no cabe", gbs(mem.total)))
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;
    use crate::detect::{parse_nvidia_smi, CpuInfo};

    #[test]
    fn estimates_unknown_models_by_size() {
        let hw = HardwareInfo {
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
            ram_available: 30_000_000_000,
            gpus: parse_nvidia_smi("RTX 4070, 12282, 12000, 1"),
            disk_free: 100_000_000_000,
        };
        assert_eq!(estimate_by_size(&hw, 5_000_000_000).0, Fit::Recommended);
        assert_eq!(estimate_by_size(&hw, 20_000_000_000).0, Fit::Possible);
        assert_eq!(estimate_by_size(&hw, 60_000_000_000).0, Fit::TooBig);
    }
}
