//! Flujo completo: runtime → descarga del modelo → test de humo → ajustes.

use std::sync::Arc;

use astrodev_core::config::{ModelSettings, ProviderConfig, ProviderKind};
use astrodev_core::Provider;
use astrodev_hardware::{Catalog, CatalogModel, Fit, HardwareInfo, Placement, RecommendOptions, Recommendation};
use tokio_util::sync::CancellationToken;

use crate::ollama::{self, OllamaClient};
use crate::runtime::RuntimeKind;
use crate::{gb, llamacpp, paths, smoke, InstallError, Reporter, Result};

pub type ProviderMaker =
    Arc<dyn Fn(&str, &ProviderConfig) -> std::result::Result<Arc<dyn Provider>, String> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub model_id: String,
    pub quant: Option<String>,
    pub runtime: RuntimeKind,
    /// Contexto preferido; si es `None` se elige el mayor que quepa.
    pub ctx: Option<u32>,
    /// Instalar aunque el recomendador diga que no cabe.
    pub force: bool,
}

/// Lo que hay que guardar en la configuración tras instalar.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub provider_name: String,
    pub provider_config: ProviderConfig,
    pub model: String,
    pub settings: ModelSettings,
    pub summary: String,
}

const CTX_CANDIDATES: &[u32] = &[32768, 16384, 8192, 4096];

fn single(catalog: &Catalog, m: &CatalogModel) -> Catalog {
    Catalog { models: vec![m.clone()], ..catalog.clone() }
}

/// Recomendación para un modelo y contexto concretos (con la cuantización pedida).
pub fn evaluate(
    hw: &HardwareInfo,
    catalog: &Catalog,
    m: &CatalogModel,
    ctx: u32,
    quant: Option<&str>,
) -> Recommendation {
    let mut c = single(catalog, m);
    if let Some(q) = quant {
        c.models[0].quants.retain(|x| x.name.eq_ignore_ascii_case(q));
        if c.models[0].quants.is_empty() {
            c.models[0].quants = m.quants.clone();
        }
    }
    astrodev_hardware::recommend(hw, &c, &RecommendOptions { ctx, ..Default::default() }).remove(0)
}

/// Contextos a probar, de mayor a menor, empezando por el mayor que (según el cálculo)
/// cabe entero en la memoria rápida.
pub fn context_candidates(hw: &HardwareInfo, catalog: &Catalog, m: &CatalogModel, quant: &str) -> Vec<u32> {
    let usable: Vec<u32> = CTX_CANDIDATES.iter().copied().filter(|c| *c <= m.context_max).collect();
    let start = usable
        .iter()
        .position(|&ctx| {
            matches!(evaluate(hw, catalog, m, ctx, Some(quant)).placement, Placement::FullGpu | Placement::Cpu)
        })
        .unwrap_or(usable.len().saturating_sub(1));
    usable[start..].to_vec()
}

pub async fn install_model(
    req: &InstallRequest,
    hw: &HardwareInfo,
    catalog: &Catalog,
    ollama_url: &str,
    make_provider: &ProviderMaker,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<InstallOutcome> {
    let m = catalog.find(&req.model_id).ok_or_else(|| {
        InstallError::Other(format!("`{}` no está en el catálogo (mira `astrodev catalog show`)", req.model_id))
    })?;
    let base = evaluate(hw, catalog, m, req.ctx.unwrap_or(8192), req.quant.as_deref());
    let quant_name = base.quant.clone();
    let q = m.quant(&quant_name).expect("cuantización del catálogo");
    if base.fit == Fit::TooBig && !req.force {
        return Err(InstallError::Other(format!(
            "{} ({}) no cabe en este equipo: {}. Elige otro en /hardware (o fuerza con --force).",
            m.name,
            quant_name,
            base.reasons.join("; ")
        )));
    }
    let free = paths::free_space(&paths::data_dir());
    if free > 0 && q.size_bytes + 1_000_000_000 > free {
        return Err(InstallError::NoSpace { needed: gb(q.size_bytes), available: gb(free) });
    }
    let mut candidates = match req.ctx {
        Some(c) => vec![c],
        None => context_candidates(hw, catalog, m, &quant_name),
    };
    if candidates.is_empty() {
        candidates.push(4096);
    }
    let has_gpu = hw.best_gpu().is_some();

    match req.runtime {
        RuntimeKind::Ollama => {
            ollama::install(hw, ollama_url, rep, cancel).await?;
            let tag = q
                .ollama_pull_name()
                .ok_or_else(|| InstallError::Other("el modelo no tiene etiqueta para Ollama".into()))?;
            rep.step(format!("Descargando {} ({}, {})", m.name, quant_name, gb(q.size_bytes)));
            let client = OllamaClient::new(ollama_url);
            client.pull(&tag, rep, cancel).await?;
            let pcfg = ProviderConfig {
                kind: ProviderKind::Ollama,
                base_url: Some(ollama_url.to_string()),
                ..Default::default()
            };
            let provider = make_provider("ollama", &pcfg).map_err(InstallError::Other)?;
            let tune = smoke::tune_ollama(&client, &provider, &tag, &candidates, has_gpu, rep, cancel).await?;
            let settings = ModelSettings {
                context_window: Some(tune.ctx),
                gpu_layers: None,
                tool_mode: None,
                measured_tps: Some((tune.smoke.tokens_per_second * 10.0).round() / 10.0),
            };
            let summary = format!(
                "{} listo en Ollama: contexto {} · {:.1} tok/s medidos{}",
                tag,
                tune.ctx,
                tune.smoke.tokens_per_second,
                if has_gpu && !tune.fully_on_gpu { " (parte en CPU)" } else { "" }
            );
            Ok(InstallOutcome { provider_name: "ollama".into(), provider_config: pcfg, model: tag, settings, summary })
        }
        RuntimeKind::LlamaCpp => {
            let bin = llamacpp::install(hw, rep, cancel).await?;
            let g =
                q.gguf.as_ref().ok_or_else(|| InstallError::Other("el modelo no tiene GGUF en el catálogo".into()))?;
            rep.step(format!("Descargando {} ({})", g.file, gb(q.size_bytes)));
            let path = llamacpp::download_gguf(&g.url(), q.sha256.as_deref(), rep, cancel).await?;
            let ctx = candidates[0];
            let r = evaluate(hw, catalog, m, ctx, Some(&quant_name));
            let gpu_layers = match r.placement {
                Placement::FullGpu => 999,
                Placement::Partial { gpu_layers, .. } => gpu_layers,
                _ => 0,
            };
            llamacpp::start_server(&bin, &path, ctx, gpu_layers, rep).await?;
            let pcfg = ProviderConfig {
                kind: ProviderKind::Openai,
                base_url: Some(format!("http://127.0.0.1:{}/v1", llamacpp::DEFAULT_PORT)),
                ..Default::default()
            };
            let provider = make_provider("llamacpp", &pcfg).map_err(InstallError::Other)?;
            let model_name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            rep.step("Test de humo");
            let s = smoke::smoke_test(&provider, &model_name, ctx, cancel).await?;
            let settings = ModelSettings {
                context_window: Some(ctx),
                gpu_layers: Some(gpu_layers),
                tool_mode: None,
                measured_tps: Some((s.tokens_per_second * 10.0).round() / 10.0),
            };
            let summary = format!(
                "{} listo en llama.cpp: contexto {ctx} · {} capas en GPU · {:.1} tok/s medidos",
                g.file,
                if gpu_layers >= 999 { "todas".to_string() } else { gpu_layers.to_string() },
                s.tokens_per_second
            );
            Ok(InstallOutcome {
                provider_name: "llamacpp".into(),
                provider_config: pcfg,
                model: model_name,
                settings,
                summary,
            })
        }
    }
}

/// Instala con Ollama un modelo que no está en el catálogo: una etiqueta de la librería de
/// Ollama (`deepseek-r1:8b`) o un GGUF de Hugging Face (`hf.co/usuario/repo:Q4_K_M`).
pub async fn install_ollama_tag(
    tag: &str,
    hw: &HardwareInfo,
    ollama_url: &str,
    make_provider: &ProviderMaker,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<InstallOutcome> {
    let tag = tag.trim();
    if tag.is_empty() || tag.contains(char::is_whitespace) {
        return Err(InstallError::Other(format!("`{tag}` no es un nombre de modelo válido")));
    }
    ollama::install(hw, ollama_url, rep, cancel).await?;
    rep.step(format!("Descargando {tag} con Ollama"));
    let client = OllamaClient::new(ollama_url);
    client.pull(tag, rep, cancel).await?;
    let pcfg =
        ProviderConfig { kind: ProviderKind::Ollama, base_url: Some(ollama_url.to_string()), ..Default::default() };
    let provider = make_provider("ollama", &pcfg).map_err(InstallError::Other)?;
    let has_gpu = hw.best_gpu().is_some();
    let tune = smoke::tune_ollama(&client, &provider, tag, &[16384, 8192, 4096], has_gpu, rep, cancel).await?;
    let settings = ModelSettings {
        context_window: Some(tune.ctx),
        measured_tps: Some((tune.smoke.tokens_per_second * 10.0).round() / 10.0),
        ..Default::default()
    };
    let summary =
        format!("{tag} listo en Ollama: contexto {} · {:.1} tok/s medidos", tune.ctx, tune.smoke.tokens_per_second);
    Ok(InstallOutcome {
        provider_name: "ollama".into(),
        provider_config: pcfg,
        model: tag.to_string(),
        settings,
        summary,
    })
}

/// Borra un modelo: GGUF local o modelo de Ollama.
pub async fn uninstall_model(name: &str, ollama_url: &str) -> Result<String> {
    let gguf = if name.ends_with(".gguf") { name.to_string() } else { format!("{name}.gguf") };
    if paths::models_dir().join(&gguf).exists() {
        llamacpp::delete_model(&gguf)?;
        return Ok(format!("Borrado {gguf}"));
    }
    OllamaClient::new(ollama_url).delete(name).await?;
    Ok(format!("Modelo {name} eliminado de Ollama"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_hardware::detect::{parse_nvidia_smi, CpuInfo};

    fn hw(vram: &str) -> HardwareInfo {
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
            ram_total: 32_000_000_000,
            ram_available: 30_000_000_000,
            gpus: parse_nvidia_smi(&format!("RTX, {vram}, {vram}, 1")),
            disk_free: 100_000_000_000,
        }
    }

    #[test]
    fn context_candidates_follow_memory() {
        let c = Catalog::embedded();
        let m = c.find("qwen2.5-coder-7b").unwrap();
        // 24 GB: cabe con 32k
        assert_eq!(context_candidates(&hw("24576"), &c, m, "Q4_K_M")[0], 32768);
        // 6 GB: hay que bajar
        let small = context_candidates(&hw("6144"), &c, m, "Q4_K_M");
        assert!(small[0] < 32768, "{small:?}");
        // nunca por encima del máximo del modelo
        let p = c.find("phi4-14b").unwrap();
        assert!(context_candidates(&hw("24576"), &c, p, "Q4_K_M").iter().all(|x| *x <= 16384));
    }

    #[tokio::test]
    async fn refuses_models_that_do_not_fit() {
        let c = Catalog::embedded();
        let req = InstallRequest {
            model_id: "qwen2.5-coder-32b".into(),
            quant: None,
            runtime: RuntimeKind::Ollama,
            ctx: None,
            force: false,
        };
        let small = HardwareInfo { ram_total: 8_000_000_000, gpus: vec![], ..hw("1") };
        let maker: ProviderMaker = Arc::new(|_, _| Err("no".into()));
        let e = install_model(
            &req,
            &small,
            &c,
            "http://127.0.0.1:9",
            &maker,
            &Reporter::silent(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("no cabe"), "{e}");
        let req = InstallRequest { model_id: "no-existe".into(), ..req };
        let e = install_model(
            &req,
            &small,
            &c,
            "http://127.0.0.1:9",
            &maker,
            &Reporter::silent(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("catálogo"));
    }
}
