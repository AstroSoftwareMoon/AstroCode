//! Binario `astrodev`: CLI, logging y ensamblado de los crates.

mod logging;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use astrodev_core::agent::ToolPolicy;
use astrodev_core::config::{self, Config};
use astrodev_core::{Agent, AgentEvent, Project, ToolContext};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(
    name = "astrodev",
    version,
    about = "Agente de programación en terminal para modelos locales y APIs"
)]
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
    /// Gestión de la configuración
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
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
    let provider_name = cli
        .provider
        .clone()
        .unwrap_or_else(|| config.default_provider.clone());
    let model = cli.model.clone().or_else(|| config.default_model.clone());
    let start = match &cli.project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let project = Arc::new(
        Project::discover(&start)
            .with_context(|| format!("no se pudo abrir {}", start.display()))?,
    );
    Ok(Runtime {
        config,
        config_path,
        provider_name,
        model,
        project,
    })
}

fn build_provider(rt: &Runtime) -> Result<Arc<dyn astrodev_core::Provider>> {
    let cfg = rt.config.provider(&rt.provider_name).with_context(|| {
        format!(
            "el proveedor `{}` no existe en {}. Disponibles: {}",
            rt.provider_name,
            rt.config_path.display(),
            rt.config
                .providers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    astrodev_providers::build(&rt.provider_name, cfg).map_err(anyhow::Error::msg)
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
        Some(Cmd::Tool {
            name,
            args,
            dry_run,
        }) => return tool_cmd(&rt, name, args, *dry_run).await,
        Some(Cmd::Models) => {
            let p = build_provider(&rt)?;
            for m in p.list_models().await? {
                let size = m
                    .size_bytes
                    .map(|b| format!("{:.1} GB", b as f64 / 1e9))
                    .unwrap_or_default();
                println!("{:<40} {size}", m.id);
            }
            return Ok(());
        }
        None => {}
    }

    let provider = build_provider(&rt)?;
    let auto_select = rt.model.is_none();
    let model = rt.model.clone().unwrap_or_default();
    let agent = Agent::new(
        provider,
        astrodev_tools::default_registry(),
        model,
        ToolContext::new(rt.project.clone()),
        rt.config.agent.clone(),
    )
    .with_policy(ToolPolicy::ReadOnly);

    if let Some(prompt) = &cli.prompt {
        return run_prompt(agent, prompt, auto_select).await;
    }

    astrodev_tui::run(astrodev_tui::TuiOptions {
        agent,
        provider_name: rt.provider_name.clone(),
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
        bail!(
            "herramienta desconocida `{name}`. Disponibles: {}",
            reg.names().join(", ")
        );
    };
    let args: serde_json::Value =
        serde_json::from_str(args).context("los argumentos deben ser JSON")?;
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
            .context(
                "el proveedor no tiene modelos; instala uno (p.ej. `ollama pull qwen2.5-coder:7b`)",
            )?;
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
