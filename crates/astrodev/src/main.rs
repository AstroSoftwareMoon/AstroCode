//! Binario `astrodev`: CLI, logging y ensamblado de los crates.

mod logging;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use astrodev_core::config::{self, Config, PermissionMode};
use astrodev_core::permissions::{PermissionReply, Permissions};
use astrodev_core::session::SessionStore;
use astrodev_core::{Agent, AgentEvent, Project, ToolContext};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(name = "astrodev", version, about = "Agente de programación en terminal para modelos locales y APIs")]
struct Cli {
    /// Proveedor a usar (clave de [providers] en config.toml)
    #[arg(long, global = true)]
    provider: Option<String>,
    /// Modelo a usar
    #[arg(short, long, global = true)]
    model: Option<String>,
    /// Directorio del proyecto (por defecto se detecta desde el actual)
    #[arg(short = 'C', long, global = true)]
    project: Option<PathBuf>,
    /// Ruta alternativa al config.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Logs detallados (nivel debug) en el archivo de log
    #[arg(long, global = true)]
    debug: bool,
    /// Ejecuta un único prompt sin TUI e imprime la respuesta
    #[arg(short, long)]
    prompt: Option<String>,
    /// Modo de permisos: ask, auto-project o auto-all
    #[arg(long, value_parser = parse_mode)]
    mode: Option<PermissionMode>,
    /// Reanuda la última sesión de este proyecto
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,
    /// Reanuda una sesión concreta (id o prefijo)
    #[arg(short, long)]
    resume: Option<String>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Lista los modelos del proveedor
    Models,
    /// Ejecuta una herramienta directamente (para probarla): astrodev tool read_file '{"path":"Cargo.toml"}'
    Tool {
        name: String,
        #[arg(default_value = "{}")]
        args: String,
        /// Sólo muestra la previsualización (diff) sin aplicar
        #[arg(long)]
        dry_run: bool,
    },
    /// Lista las herramientas disponibles
    Tools,
    /// Muestra el hardware detectado (CPU, RAM, GPU/VRAM, disco)
    Hardware {
        #[arg(long)]
        json: bool,
    },
    /// Recomienda modelos locales para este equipo
    Recommend {
        /// Contexto deseado en tokens
        #[arg(long, default_value_t = 8192)]
        ctx: u32,
        /// Incluye también los que no caben
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Catálogo de modelos
    Catalog {
        #[command(subcommand)]
        action: CatalogCmd,
    },
    /// Lista las sesiones guardadas de este proyecto (--all para todas)
    Sessions {
        #[arg(long)]
        all: bool,
    },
    /// Gestión de la configuración
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
}

#[derive(Subcommand, Debug)]
enum CatalogCmd {
    /// Descarga el catálogo más reciente
    Update,
    /// Lista los modelos del catálogo
    Show,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Muestra la ruta del config.toml
    Path,
    /// Crea un config.toml por defecto si no existe
    Init,
    /// Muestra la configuración efectiva
    Show,
}

struct Runtime {
    config: Config,
    config_path: PathBuf,
    provider_name: String,
    model: Option<String>,
    project: Arc<Project>,
}

fn load_runtime(cli: &Cli) -> Result<Runtime> {
    let config_path = match &cli.config {
        Some(p) => p.clone(),
        None => Config::default_path()?,
    };
    let (config, _first_run) = Config::load_or_default(&config_path)?;
    let provider_name = cli.provider.clone().unwrap_or_else(|| config.default_provider.clone());
    let model = cli.model.clone().or_else(|| config.default_model.clone());
    let start = match &cli.project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let project = Arc::new(Project::discover(&start).with_context(|| format!("no se pudo abrir {}", start.display()))?);
    Ok(Runtime { config, config_path, provider_name, model, project })
}

fn parse_mode(s: &str) -> Result<PermissionMode, String> {
    match s {
        "ask" => Ok(PermissionMode::Ask),
        "auto-project" => Ok(PermissionMode::AutoProject),
        "auto-all" => Ok(PermissionMode::AutoAll),
        _ => Err("usa ask, auto-project o auto-all".into()),
    }
}

/// Construye un proveedor por nombre (lo usa también la TUI para /model prov/modelo).
fn provider_factory(cfg: &Config, name: &str) -> Result<Arc<dyn astrodev_core::Provider>, String> {
    let p = cfg.provider(name).ok_or_else(|| {
        format!(
            "el proveedor `{name}` no existe. Disponibles: {}",
            cfg.providers.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    astrodev_providers::build(name, p)
}

fn build_provider(rt: &Runtime) -> Result<Arc<dyn astrodev_core::Provider>> {
    provider_factory(&rt.config, &rt.provider_name).map_err(anyhow::Error::msg)
}

/// Crea el agente con toda la configuración aplicada.
fn build_agent(cli: &Cli, rt: &Runtime) -> Result<(Agent, bool)> {
    let provider = build_provider(rt)?;
    let model = rt.model.clone().unwrap_or_default();
    let mut perms_cfg = rt.config.permissions.clone();
    if let Some(m) = cli.mode {
        perms_cfg.mode = m;
    }
    let pcfg = rt.config.provider(&rt.provider_name).cloned().unwrap_or_default();
    let msettings = rt.config.models.get(&model).cloned().unwrap_or_default();
    let mut agent = Agent::new(
        provider,
        astrodev_tools::default_registry(),
        model,
        ToolContext::new(rt.project.clone()),
        rt.config.agent.clone(),
    )
    .with_provider_name(rt.provider_name.clone())
    .with_permissions(Permissions::new(&perms_cfg))
    .with_tool_mode(msettings.tool_mode.unwrap_or(pcfg.tool_mode));
    if let Some(cw) = msettings.context_window {
        agent = agent.with_context_window(cw);
    }
    if let Some(store) = SessionStore::default_store() {
        if cli.continue_last || cli.resume.is_some() {
            let session = match &cli.resume {
                Some(id) => Some(store.load(id).with_context(|| format!("no se pudo cargar la sesión {id}"))?),
                None => store.latest(rt.project.root()),
            };
            match session {
                Some(s) => agent.load_session(s),
                None => eprintln!("No hay sesiones previas en este proyecto; empiezo una nueva."),
            }
        }
        agent = agent.with_store(store);
    }
    let auto_select = agent.model().is_empty();
    Ok((agent, auto_select))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let interactive = cli.prompt.is_none() && cli.command.is_none();
    let _guard = logging::init(cli.debug, interactive);
    if let Err(e) = real_main(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn real_main(cli: Cli) -> Result<()> {
    let rt = load_runtime(&cli)?;
    tracing::info!(project = %rt.project.root().display(), provider = %rt.provider_name, "inicio");

    match &cli.command {
        Some(Cmd::Config { action }) => return config_cmd(action, &rt),
        Some(Cmd::Tools) => {
            for s in astrodev_tools::default_registry().specs() {
                println!("{:<12} {}", s.name, s.description);
            }
            return Ok(());
        }
        Some(Cmd::Tool { name, args, dry_run }) => return tool_cmd(&rt, name, args, *dry_run).await,
        Some(Cmd::Sessions { all }) => {
            let store = SessionStore::default_store().context("sin directorio de datos")?;
            let list = store.list(if *all { None } else { Some(rt.project.root()) });
            if list.is_empty() {
                println!("No hay sesiones guardadas.");
            }
            for s in list {
                println!(
                    "{}  {}  {:>3} msgs  {:<24} {}",
                    s.id,
                    s.updated_at.format("%Y-%m-%d %H:%M"),
                    s.messages,
                    s.model,
                    s.title
                );
            }
            println!("\nReanuda con: astrodev --resume <id>   (o -c para la última)");
            return Ok(());
        }
        Some(Cmd::Hardware { json }) => return hardware_cmd(*json),
        Some(Cmd::Recommend { ctx, all, json }) => return recommend_cmd(&rt, *ctx, *all, *json).await,
        Some(Cmd::Catalog { action }) => return catalog_cmd(&rt, action).await,
        Some(Cmd::Models) => {
            let p = build_provider(&rt)?;
            for m in p.list_models().await? {
                let size = m.size_bytes.map(|b| format!("{:.1} GB", b as f64 / 1e9)).unwrap_or_default();
                println!("{:<40} {size}", m.id);
            }
            return Ok(());
        }
        None => {}
    }

    let (agent, auto_select) = build_agent(&cli, &rt)?;

    if let Some(prompt) = &cli.prompt {
        return run_prompt(agent, prompt, auto_select).await;
    }

    astrodev_tui::run(astrodev_tui::TuiOptions {
        agent,
        config: rt.config.clone(),
        config_path: rt.config_path.clone(),
        factory: Arc::new(provider_factory),
        auto_select_model: auto_select,
    })
    .await
}

fn config_cmd(action: &ConfigCmd, rt: &Runtime) -> Result<()> {
    match action {
        ConfigCmd::Path => println!("{}", rt.config_path.display()),
        ConfigCmd::Init => {
            if rt.config_path.exists() {
                println!("Ya existe: {}", rt.config_path.display());
            } else {
                Config::default().save(&rt.config_path)?;
                println!("Creado: {}", rt.config_path.display());
            }
        }
        ConfigCmd::Show => {
            println!("# {}", rt.config_path.display());
            println!("# datos: {}", config::data_dir()?.display());
            print!("{}", rt.config.to_toml()?);
        }
    }
    Ok(())
}

async fn tool_cmd(rt: &Runtime, name: &str, args: &str, dry_run: bool) -> Result<()> {
    let reg = astrodev_tools::default_registry();
    let Some(tool) = reg.get(name) else {
        bail!("herramienta desconocida `{name}`. Disponibles: {}", reg.names().join(", "));
    };
    let args: serde_json::Value = serde_json::from_str(args).context("los argumentos deben ser JSON")?;
    let ctx = ToolContext::new(rt.project.clone());
    if dry_run {
        match tool.preview(&args, &ctx).await? {
            Some(p) if !p.is_empty() => println!("{p}"),
            _ => println!("(sin previsualización / sin cambios)"),
        }
        return Ok(());
    }
    let out = tool.execute(&args, &ctx).await?;
    if let Some(d) = out.diff.as_deref().filter(|d| !d.is_empty()) {
        println!("{d}");
    }
    println!("{}", out.content);
    Ok(())
}

async fn run_prompt(mut agent: Agent, prompt: &str, auto_select: bool) -> Result<()> {
    if auto_select {
        let models = agent.provider().list_models().await?;
        let pick = models
            .iter()
            .find(|m| m.id.contains("code"))
            .or(models.first())
            .context("el proveedor no tiene modelos; instala uno (p.ej. `ollama pull qwen2.5-coder:7b`)")?;
        eprintln!("[modelo: {}]", pick.id);
        agent.set_model(pick.id.clone());
    }
    let (tx, mut rx) = mpsc::channel(256);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c2.cancel();
        }
    });
    let prompt = prompt.to_string();
    let handle = tokio::spawn(async move {
        agent.run_turn(prompt, tx, cancel).await;
    });
    let mut failed = None;
    let mut out = std::io::stdout();
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::TextDelta(t) => {
                print!("{t}");
                let _ = out.flush();
            }
            AgentEvent::ToolStart(c) => eprintln!("\n[herramienta] {} {}", c.name, c.arguments),
            AgentEvent::ToolEnd { result: Err(e), .. } => eprintln!("[error] {e}"),
            AgentEvent::ToolEnd { result: Ok(o), .. } => {
                if let Some(d) = o.diff.as_deref().filter(|d| !d.is_empty()) {
                    eprintln!("{d}");
                }
            }
            AgentEvent::Notice(n) => eprintln!("[aviso] {n}"),
            AgentEvent::PermissionRequest(req) => {
                let reply = ask_stdin(&req);
                req.respond(reply);
            }
            AgentEvent::Error(e) => failed = Some(e),
            AgentEvent::Cancelled => failed = Some("cancelado".into()),
            _ => {}
        }
    }
    handle.await?;
    println!();
    if let Some(e) = failed {
        bail!(e);
    }
    Ok(())
}

/// Pregunta en la terminal (modo `-p`). Sin terminal interactiva, deniega.
fn ask_stdin(req: &astrodev_core::agent::PermissionRequest) -> PermissionReply {
    use std::io::IsTerminal;
    eprintln!("\n── permiso requerido: {} — {}", req.call.name, req.reason);
    if let Some(p) = req.preview.as_deref().filter(|p| !p.is_empty()) {
        eprintln!("{p}");
    } else {
        eprintln!("{}", serde_json::to_string_pretty(&req.call.arguments).unwrap_or_default());
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("(sin terminal interactiva: denegado; usa --mode auto-project o auto-all)");
        return PermissionReply::Deny { feedback: None };
    }
    eprint!("¿Permitir? [s]í / [a] siempre en esta sesión / [n]o: ");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    match line.trim().to_lowercase().as_str() {
        "s" | "si" | "sí" | "y" | "yes" => PermissionReply::AllowOnce,
        "a" if !req.dangerous => PermissionReply::AllowSession,
        _ => PermissionReply::Deny { feedback: None },
    }
}

fn data_dir() -> PathBuf {
    config::data_dir().unwrap_or_else(|_| std::env::temp_dir().join("astrodev"))
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

fn hardware_cmd(json: bool) -> Result<()> {
    let hw = astrodev_hardware::detect(&data_dir().join("models"));
    if json {
        println!("{}", serde_json::to_string_pretty(&hw)?);
        return Ok(());
    }
    println!("Sistema  {} / {}", hw.os, hw.arch);
    println!(
        "CPU      {} — {} núcleos / {} hilos{}{}",
        hw.cpu.brand,
        hw.cpu.physical_cores,
        hw.cpu.logical_cores,
        if hw.cpu.avx2 { " · AVX2" } else { "" },
        if hw.cpu.avx512 { " · AVX-512" } else { "" }
    );
    println!("RAM      {} total · {} disponible", gb(hw.ram_total), gb(hw.ram_available));
    if hw.gpus.is_empty() {
        println!("GPU      ninguna compatible detectada (se usará la CPU)");
    }
    for g in &hw.gpus {
        println!(
            "GPU      {} — {} {}{}",
            g.name,
            gb(g.vram_total),
            if g.unified { "memoria unificada" } else { "VRAM" },
            g.vram_free.map(|f| format!(" ({} libre)", gb(f))).unwrap_or_default()
        );
    }
    println!("Disco    {} libres en {}", gb(hw.disk_free), data_dir().display());
    println!("Backend  {}", hw.backend().label());
    Ok(())
}

async fn load_catalog(rt: &Runtime) -> astrodev_hardware::Catalog {
    let data = data_dir();
    if astrodev_hardware::Catalog::cache_is_stale(&data) {
        let url = rt.config.catalog_url.clone().unwrap_or_else(|| astrodev_hardware::catalog::DEFAULT_URL.into());
        if let Err(e) = astrodev_hardware::Catalog::update(&url, &data).await {
            tracing::info!("catálogo no actualizado: {e}");
        }
    }
    astrodev_hardware::Catalog::load(&data)
}

async fn recommend_cmd(rt: &Runtime, ctx: u32, all: bool, json: bool) -> Result<()> {
    use astrodev_hardware::Fit;
    let hw = astrodev_hardware::detect(&data_dir().join("models"));
    let catalog = load_catalog(rt).await;
    let recs =
        astrodev_hardware::recommend(&hw, &catalog, &astrodev_hardware::RecommendOptions { ctx, ..Default::default() });
    let recs: Vec<_> = recs.into_iter().filter(|r| all || r.fit != Fit::TooBig).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&recs)?);
        return Ok(());
    }
    let best = hw.best_gpu().map(|g| format!("{} ({})", g.name, gb(g.vram_total))).unwrap_or_else(|| "sin GPU".into());
    println!("Equipo: {} · {} RAM · {}   (contexto {ctx})\n", hw.cpu.brand, gb(hw.ram_total), best);
    println!("{:<2} {:<34} {:<7} {:>9} {:>9} {:>6}  motivo", "", "modelo", "cuant.", "descarga", "memoria", "tok/s");
    for r in &recs {
        let icon = match r.fit {
            Fit::Recommended => "●",
            Fit::Possible => "◐",
            Fit::TooBig => "○",
        };
        println!(
            "{icon:<2} {:<34} {:<7} {:>9} {:>9} {:>6}  {} [{}]",
            format!("{}{}", r.model_name, if r.supports_tools { "" } else { " *" }),
            r.quant,
            gb(r.download_bytes),
            gb(r.memory.total),
            if r.tokens_per_second > 0.0 { format!("{:.0}", r.tokens_per_second) } else { "—".into() },
            r.reasons.first().cloned().unwrap_or_default(),
            r.fit.label()
        );
    }
    println!("\n● recomendado  ◐ posible (lento o repartido GPU/CPU)  ○ demasiado grande   * sin tools nativas");
    if let Some(top) = recs.first() {
        println!("Instala el primero con: astrodev pull {}", top.model_id);
    }
    Ok(())
}

async fn catalog_cmd(rt: &Runtime, action: &CatalogCmd) -> Result<()> {
    match action {
        CatalogCmd::Update => {
            let url = rt.config.catalog_url.clone().unwrap_or_else(|| astrodev_hardware::catalog::DEFAULT_URL.into());
            let c = astrodev_hardware::Catalog::update(&url, &data_dir()).await.map_err(anyhow::Error::msg)?;
            println!("Catálogo actualizado: versión {} ({} modelos)", c.version, c.models.len());
        }
        CatalogCmd::Show => {
            let c = astrodev_hardware::Catalog::load(&data_dir());
            println!("Catálogo versión {} ({})", c.version, c.updated);
            for m in &c.models {
                let quants: Vec<&str> = m.quants.iter().map(|q| q.name.as_str()).collect();
                println!(
                    "{:<26} {:<34} {:>5.1}B  código {:>3}  {}",
                    m.id,
                    m.name,
                    m.params_b,
                    m.code_score,
                    quants.join(",")
                );
            }
        }
    }
    Ok(())
}
