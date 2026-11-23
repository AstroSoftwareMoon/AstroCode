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
    /// Abre el asistente de configuración inicial
    #[arg(long)]
    setup: bool,
    /// Reanuda una sesión concreta (id o prefijo)
    #[arg(short, long)]
    resume: Option<String>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Lista los modelos instalados (del proveedor y GGUF de llama.cpp)
    Models,
    /// Busca modelos en el catálogo y dice si caben en este equipo
    Search {
        /// Texto a buscar (nombre, familia, tamaño…)
        query: Vec<String>,
        #[arg(long, default_value_t = 8192)]
        ctx: u32,
    },
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
    /// Instala un runtime local: ollama o llamacpp
    Install { runtime: String },
    /// Descarga e instala un modelo del catálogo (con test de humo y auto-ajuste)
    Pull {
        /// id del catálogo (p.ej. qwen2.5-coder-7b) o etiqueta de Ollama
        model: String,
        /// Cuantización (Q4_K_M, Q5_K_M, Q8_0…)
        #[arg(long)]
        quant: Option<String>,
        /// Runtime: ollama (por defecto) o llamacpp
        #[arg(long, default_value = "ollama")]
        runtime: String,
        /// Contexto fijo (si no, se elige el mayor que quepa)
        #[arg(long)]
        ctx: Option<u32>,
        /// Instalar aunque no quepa
        #[arg(long)]
        force: bool,
        /// No convertirlo en el modelo por defecto
        #[arg(long)]
        no_default: bool,
    },
    /// Borra un modelo descargado (de Ollama o un .gguf)
    Rm { model: String },
    /// Desinstala un runtime instalado por AstroDev: ollama o llamacpp
    Uninstall { runtime: String },
    /// Prueba rápida del modelo actual: responde y mide tok/s
    SmokeTest,
    /// Proveedores de modelos (APIs y servidores locales)
    Provider {
        #[command(subcommand)]
        action: ProviderCmd,
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
enum ProviderCmd {
    /// Asistente para añadir un proveedor (la clave se guarda en el keyring)
    Add {
        /// Preset: anthropic, openai, google, openrouter, groq, deepseek, mistral, ollama, lmstudio, llamacpp, vllm, custom
        preset: Option<String>,
        /// Nombre en la configuración (por defecto el del preset)
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Modelo por defecto
        #[arg(long)]
        model: Option<String>,
        /// No convertirlo en el proveedor por defecto
        #[arg(long)]
        no_default: bool,
    },
    /// Lista los proveedores configurados
    List,
    /// Elimina un proveedor (y su clave del keyring)
    Remove { name: String },
    /// Comprueba la conexión y lista sus modelos
    Test { name: String },
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
    first_run: bool,
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
    let (config, first_run) = Config::load_or_default(&config_path)?;
    // `-m proveedor/modelo`, o el perfil `edit`, o el modelo por defecto.
    let spec = cli.model.clone().or_else(|| config.profiles.edit.clone());
    let (spec_provider, spec_model) = match &spec {
        Some(s) => {
            let (p, m) = config::split_model_ref(&config, s);
            (p.map(str::to_string), Some(m.to_string()))
        }
        None => (None, None),
    };
    let provider_name = cli.provider.clone().or(spec_provider).unwrap_or_else(|| config.default_provider.clone());
    let model = spec_model.or_else(|| config.default_model.clone());
    let start = match &cli.project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let project = Arc::new(Project::discover(&start).with_context(|| format!("no se pudo abrir {}", start.display()))?);
    Ok(Runtime { first_run, config, config_path, provider_name, model, project })
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
    // Perfiles por tarea y fallback.
    let resolve = |spec: &str| -> Option<astrodev_core::agent::ModelRef> {
        let (p, m) = config::split_model_ref(&rt.config, spec);
        let pname = p.unwrap_or(&rt.provider_name).to_string();
        match provider_factory(&rt.config, &pname) {
            Ok(prov) => Some(astrodev_core::agent::ModelRef::new(pname, prov, m)),
            Err(e) => {
                eprintln!("aviso: perfil `{spec}` ignorado: {e}");
                None
            }
        }
    };
    let profiles = &rt.config.profiles;
    agent = agent
        .with_planner(profiles.plan.as_deref().and_then(resolve))
        .with_summarizer(profiles.summarize.as_deref().and_then(resolve))
        .with_fallbacks(profiles.fallback.iter().filter_map(|s| resolve(s)).collect());
    let cost_cfg = rt.config.clone();
    agent = agent.with_cost_fn(Arc::new(move |prov: &str, model: &str, u| {
        astrodev_providers::pricing::cost_for(&cost_cfg, prov, model, u)
    }));
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
    // `astrodev recommend | head` no debe hacer pánico al cerrarse la tubería.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
        Some(Cmd::Provider { action }) => return provider_cmd(&rt, action).await,
        Some(Cmd::Install { runtime }) => return install_cmd(&rt, runtime).await,
        Some(Cmd::Pull { model, quant, runtime, ctx, force, no_default }) => {
            return pull_cmd(&rt, model, quant.clone(), runtime, *ctx, *force, *no_default).await
        }
        Some(Cmd::Rm { model }) => {
            let s = astrodev_installer::pipeline::uninstall_model(model, &ollama_url(&rt.config)).await?;
            println!("{s}");
            return Ok(());
        }
        Some(Cmd::Uninstall { runtime }) => {
            let s = match astrodev_installer::runtime::RuntimeKind::parse(runtime) {
                Some(astrodev_installer::runtime::RuntimeKind::Ollama) => {
                    astrodev_installer::ollama::uninstall_managed()?
                }
                Some(astrodev_installer::runtime::RuntimeKind::LlamaCpp) => {
                    astrodev_installer::llamacpp::uninstall_managed()?
                }
                None => bail!("runtime desconocido `{runtime}`: usa ollama o llamacpp"),
            };
            println!("{s}");
            return Ok(());
        }
        Some(Cmd::SmokeTest) => {
            let p = build_provider(&rt)?;
            let model = match &rt.model {
                Some(m) => m.clone(),
                None => p.list_models().await?.first().map(|m| m.id.clone()).context("no hay modelos")?,
            };
            let ctx = rt.config.models.get(&model).and_then(|m| m.context_window).unwrap_or(4096);
            eprintln!("Probando {}/{model} (contexto {ctx})…", rt.provider_name);
            let r = astrodev_installer::smoke::smoke_test(&p, &model, ctx, &CancellationToken::new()).await?;
            println!("respuesta: {}", r.reply.chars().take(200).collect::<String>());
            println!("velocidad: {:.1} tok/s · primer token en {} ms", r.tokens_per_second, r.first_token_ms);
            return Ok(());
        }
        Some(Cmd::Models) => {
            let hw = astrodev_hardware::detect(&data_dir().join("models"));
            let p = build_provider(&rt)?;
            println!("{}:", rt.provider_name);
            match p.list_models().await {
                Ok(ms) if !ms.is_empty() => {
                    for m in ms {
                        let fit = m
                            .size_bytes
                            .map(|b| astrodev_hardware::recommend::estimate_by_size(&hw, b).1)
                            .unwrap_or_default();
                        let size = m.size_bytes.map(gb).unwrap_or_default();
                        let mark = if rt.model.as_deref() == Some(m.id.as_str()) { "●" } else { " " };
                        println!("{mark} {:<44} {size:>9}  {fit}", m.id);
                    }
                }
                Ok(_) => println!("  (ninguno)"),
                Err(e) => println!("  ✗ {e}"),
            }
            let ggufs = astrodev_installer::llamacpp::list_models();
            if !ggufs.is_empty() {
                println!("\nGGUF para llama.cpp ({}):", astrodev_installer::paths::models_dir().display());
                for (name, size) in ggufs {
                    println!("  {name:<44} {:>9}", gb(size));
                }
            }
            return Ok(());
        }
        Some(Cmd::Search { query, ctx }) => return search_cmd(&rt, &query.join(" "), *ctx).await,
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
        first_run: (rt.first_run || cli.setup) && cli.model.is_none(),
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

fn ollama_url(cfg: &Config) -> String {
    cfg.providers
        .values()
        .find(|p| p.kind == config::ProviderKind::Ollama)
        .and_then(|p| p.base_url.clone())
        .unwrap_or_else(|| astrodev_installer::ollama::DEFAULT_URL.into())
}

/// Imprime los eventos del instalador en la terminal (barra de progreso en stderr).
fn spawn_progress_printer(
    mut rx: mpsc::UnboundedReceiver<astrodev_installer::InstallEvent>,
) -> tokio::task::JoinHandle<()> {
    use astrodev_installer::InstallEvent;
    tokio::spawn(async move {
        let mut in_bar = false;
        while let Some(e) = rx.recv().await {
            let clear = |in_bar: &mut bool| {
                if *in_bar {
                    eprintln!();
                    *in_bar = false;
                }
            };
            match e {
                InstallEvent::Step(s) => {
                    clear(&mut in_bar);
                    eprintln!("▸ {s}");
                }
                InstallEvent::Log(l) => {
                    clear(&mut in_bar);
                    eprintln!("  {l}");
                }
                InstallEvent::Progress { label, done, total, bytes_per_sec } => {
                    let line = match total {
                        Some(t) if t > 0 => {
                            let frac = done as f64 / t as f64;
                            let w = 30;
                            let full = (frac * w as f64) as usize;
                            format!(
                                "  [{}{}] {:>3.0}%  {} / {}  {:.1} MB/s  {label}",
                                "█".repeat(full),
                                "░".repeat(w - full.min(w)),
                                frac * 100.0,
                                gb(done),
                                gb(t),
                                bytes_per_sec / 1e6
                            )
                        }
                        _ => format!("  {}  {label}", gb(done)),
                    };
                    eprint!("\r{line}\x1b[K");
                    in_bar = true;
                }
                InstallEvent::Done(s) => {
                    clear(&mut in_bar);
                    eprintln!("✓ {s}");
                }
                InstallEvent::Error(s) => {
                    clear(&mut in_bar);
                    eprintln!("✗ {s}");
                }
            }
        }
    })
}

fn ctrl_c_token() -> CancellationToken {
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c2.cancel();
        }
    });
    cancel
}

async fn install_cmd(rt: &Runtime, runtime: &str) -> Result<()> {
    use astrodev_installer::runtime::RuntimeKind;
    let kind = RuntimeKind::parse(runtime).context("usa `ollama` o `llamacpp`")?;
    let hw = astrodev_hardware::detect(&data_dir().join("models"));
    let (tx, rx) = mpsc::unbounded_channel();
    let printer = spawn_progress_printer(rx);
    let rep = astrodev_installer::Reporter::new(tx);
    let cancel = ctrl_c_token();
    let res = match kind {
        RuntimeKind::Ollama => astrodev_installer::ollama::install(&hw, &ollama_url(&rt.config), &rep, &cancel)
            .await
            .map(|s| format!("Ollama {} listo", s.version.unwrap_or_default())),
        RuntimeKind::LlamaCpp => astrodev_installer::llamacpp::install(&hw, &rep, &cancel)
            .await
            .map(|b| format!("llama.cpp listo: {}", b.display())),
    };
    drop(rep);
    let _ = printer.await;
    println!("{}", res?);
    Ok(())
}

async fn pull_cmd(
    rt: &Runtime,
    model: &str,
    quant: Option<String>,
    runtime: &str,
    ctx: Option<u32>,
    force: bool,
    no_default: bool,
) -> Result<()> {
    use astrodev_installer::pipeline::{install_model, InstallRequest, ProviderMaker};
    let kind = astrodev_installer::runtime::RuntimeKind::parse(runtime).context("runtime: ollama o llamacpp")?;
    let hw = astrodev_hardware::detect(&data_dir().join("models"));
    let catalog = load_catalog(rt).await;
    let maker: ProviderMaker = Arc::new(|name: &str, pcfg: &config::ProviderConfig| {
        let mut c = Config::default();
        c.providers.insert(name.to_string(), pcfg.clone());
        provider_factory(&c, name)
    });
    let req = InstallRequest { model_id: model.to_string(), quant, runtime: kind, ctx, force };
    let (tx, rx) = mpsc::unbounded_channel();
    let printer = spawn_progress_printer(rx);
    let rep = astrodev_installer::Reporter::new(tx);
    let res = if catalog.find(model).is_none() {
        // Fuera del catálogo: etiqueta de Ollama (deepseek-r1:8b) o hf.co/usuario/repo:QUANT.
        astrodev_installer::pipeline::install_ollama_tag(
            model,
            &hw,
            &ollama_url(&rt.config),
            &maker,
            &rep,
            &ctrl_c_token(),
        )
        .await
    } else {
        install_model(&req, &hw, &catalog, &ollama_url(&rt.config), &maker, &rep, &ctrl_c_token()).await
    };
    drop(rep);
    let _ = printer.await;
    let o = res?;
    let mut cfg = rt.config.clone();
    cfg.providers.entry(o.provider_name.clone()).or_insert(o.provider_config.clone());
    cfg.models.insert(o.model.clone(), o.settings.clone());
    if !no_default {
        cfg.default_provider = o.provider_name.clone();
        cfg.default_model = Some(o.model.clone());
    }
    cfg.save(&rt.config_path)?;
    println!("✓ {}", o.summary);
    if !no_default {
        println!("Es tu modelo por defecto. Ejecuta `astrodev` para empezar.");
    }
    Ok(())
}

fn prompt_line(label: &str, default: Option<&str>) -> Result<String> {
    use std::io::BufRead;
    match default {
        Some(d) if !d.is_empty() => eprint!("{label} [{d}]: "),
        _ => eprint!("{label}: "),
    }
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    let s = s.trim().to_string();
    Ok(if s.is_empty() { default.unwrap_or("").to_string() } else { s })
}

async fn provider_cmd(rt: &Runtime, action: &ProviderCmd) -> Result<()> {
    use astrodev_providers::{keys, presets};
    let mut cfg = rt.config.clone();
    match action {
        ProviderCmd::List => {
            for (name, p) in &cfg.providers {
                let key = match &p.api_key_ref {
                    Some(r) => format!("clave: keyring {r}"),
                    None => {
                        presets::env_key_for(p).map(|e| format!("clave: ${e}")).unwrap_or_else(|| "sin clave".into())
                    }
                };
                let mark = if *name == cfg.default_provider { "●" } else { " " };
                println!(
                    "{mark} {name:<14} {:<10} {:<45} {key}",
                    format!("{:?}", p.kind).to_lowercase(),
                    p.base_url.clone().unwrap_or_default()
                );
            }
        }
        ProviderCmd::Remove { name } => {
            let p = cfg.providers.remove(name).with_context(|| format!("no existe el proveedor {name}"))?;
            if let Some(r) = p.api_key_ref {
                let _ = tokio::task::spawn_blocking(move || keys::keyring_delete(&r)).await;
            }
            if cfg.default_provider == *name {
                cfg.default_provider = cfg.providers.keys().next().cloned().unwrap_or_default();
                cfg.default_model = None;
            }
            cfg.save(&rt.config_path)?;
            println!("Proveedor {name} eliminado.");
        }
        ProviderCmd::Test { name } => {
            let p = provider_factory(&cfg, name).map_err(anyhow::Error::msg)?;
            let models = p.list_models().await?;
            println!("✓ {name} responde: {} modelos", models.len());
            for m in models.iter().take(30) {
                println!("  {}", m.id);
            }
        }
        ProviderCmd::Add { preset, name, base_url, model, no_default } => {
            let preset = match preset {
                Some(p) => presets::find(p).with_context(|| format!("preset desconocido `{p}`"))?,
                None => {
                    eprintln!("Proveedores disponibles:");
                    for (i, p) in presets::PRESETS.iter().enumerate() {
                        eprintln!("  {:>2}) {:<12} {}", i + 1, p.id, p.label);
                    }
                    let c = prompt_line("Elige número o nombre", Some("1"))?;
                    c.parse::<usize>()
                        .ok()
                        .and_then(|i| presets::PRESETS.get(i.wrapping_sub(1)))
                        .or_else(|| presets::find(&c))
                        .context("opción no válida")?
                }
            };
            let name = match name {
                Some(n) => n.clone(),
                None => prompt_line("Nombre", Some(preset.id))?,
            };
            let url = match base_url {
                Some(u) => u.clone(),
                None => prompt_line("URL base", Some(preset.base_url))?,
            };
            if url.is_empty() {
                bail!("hace falta una URL base");
            }
            let mut key_ref = None;
            if preset.needs_key {
                let env_has = preset.env_key.and_then(|e| std::env::var(e).ok()).is_some();
                eprint!(
                    "Clave de API{} (no se muestra; se guarda en el keyring del sistema): ",
                    if env_has { " (Enter para usar la variable de entorno)" } else { "" }
                );
                std::io::stderr().flush().ok();
                let key = rpassword::read_password().unwrap_or_default();
                if !key.trim().is_empty() {
                    let r = name.clone();
                    let k = key.trim().to_string();
                    let saved = tokio::task::spawn_blocking(move || keys::keyring_set(&r, &k)).await?;
                    match saved {
                        Ok(()) => {
                            eprintln!("✓ Clave guardada en el keyring ({}/{name})", keys::SERVICE);
                            key_ref = Some(name.clone());
                        }
                        Err(e) => bail!(e),
                    }
                } else if !env_has {
                    eprintln!(
                        "Sin clave: AstroDev la buscará en {}.",
                        preset.env_key.unwrap_or("la variable de entorno")
                    );
                }
            }
            let mut pcfg = preset.config(&url, key_ref);
            cfg.providers.insert(name.clone(), pcfg.clone());
            eprintln!("Comprobando conexión…");
            let models = match provider_factory(&cfg, &name).map_err(anyhow::Error::msg)?.list_models().await {
                Ok(m) => m.into_iter().map(|m| m.id).collect::<Vec<_>>(),
                Err(e) => {
                    eprintln!("✗ {e}");
                    Vec::new()
                }
            };
            if !models.is_empty() {
                eprintln!(
                    "✓ {} modelos disponibles: {}{}",
                    models.len(),
                    models.iter().take(8).cloned().collect::<Vec<_>>().join(", "),
                    if models.len() > 8 { "…" } else { "" }
                );
            }
            let suggested =
                model.clone().or_else(|| preset.default_model.map(str::to_string)).or_else(|| models.first().cloned());
            let chosen = match model {
                Some(m) => m.clone(),
                None => prompt_line("Modelo por defecto", suggested.as_deref())?,
            };
            if !chosen.is_empty() {
                pcfg.default_model = Some(chosen.clone());
                cfg.providers.insert(name.clone(), pcfg);
            }
            if !no_default && !chosen.is_empty() {
                cfg.default_provider = name.clone();
                cfg.default_model = Some(chosen.clone());
            }
            cfg.save(&rt.config_path)?;
            println!("✓ Proveedor `{name}` añadido{}.", if !no_default { " y marcado por defecto" } else { "" });
        }
    }
    Ok(())
}

async fn search_cmd(rt: &Runtime, query: &str, ctx: u32) -> Result<()> {
    let hw = astrodev_hardware::detect(&data_dir().join("models"));
    let catalog = load_catalog(rt).await;
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
    let recs =
        astrodev_hardware::recommend(&hw, &catalog, &astrodev_hardware::RecommendOptions { ctx, ..Default::default() });
    let hits: Vec<_> = recs
        .iter()
        .filter(|r| {
            let hay =
                format!("{} {} {}", r.model_name.to_lowercase(), r.model_id, r.ollama_pull.clone().unwrap_or_default());
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect();
    if hits.is_empty() {
        println!("Nada en el catálogo para «{query}».");
    }
    for r in &hits {
        let icon = match r.fit {
            astrodev_hardware::Fit::Recommended => "●",
            astrodev_hardware::Fit::Possible => "◐",
            astrodev_hardware::Fit::TooBig => "○",
        };
        println!(
            "{icon} {:<34} {:<7} {:>9}  {:<22} {}",
            r.model_name,
            r.quant,
            gb(r.download_bytes),
            r.model_id,
            r.reasons.first().cloned().unwrap_or_default()
        );
    }
    if !query.is_empty() && !query.contains(' ') {
        println!(
            "\nTambién puedes descargar cualquier modelo de Ollama o un GGUF de Hugging Face:\n  astrodev pull {query}"
        );
    }
    Ok(())
}
