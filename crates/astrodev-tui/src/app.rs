//! Estado de la aplicación y manejo de eventos.

use std::path::PathBuf;
use std::sync::Arc;

use astrodev_core::agent::{ModelRef, PermissionRequest};
use astrodev_core::config::{Config, PermissionMode};
use astrodev_core::message::Role;
use astrodev_core::permissions::PermissionReply;
use astrodev_core::provider::Usage;
use astrodev_core::session::SessionMeta;
use astrodev_core::{Agent, AgentEvent, Message, Provider, ToolCall};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::commands::{self, Command};
use crate::hardware_view::{HardwareView, HwAction, InstalledModel, Source};
use crate::input::Input;
use crate::install_view::InstallView;
use crate::onboarding_view::{OnboardAction, Onboarding};
use crate::provider_view::{ProviderWizard, WizardAction};
use astrodev_installer::pipeline::{InstallOutcome, InstallRequest, ProviderMaker};
use astrodev_installer::runtime::RuntimeKind;
use astrodev_installer::{InstallEvent, Reporter};

/// Construye un proveedor a partir de su nombre en la configuración.
pub type ProviderFactory = Arc<dyn Fn(&Config, &str) -> Result<Arc<dyn Provider>, String> + Send + Sync>;

/// Mensajes asíncronos hacia la aplicación.
#[derive(Debug)]
pub enum AppMsg {
    Agent(AgentEvent),
    TurnFinished,
    Info(String),
    Error(String),
    /// Resultado de listar modelos. `auto_select` = elegir uno si no hay modelo.
    Models {
        models: Vec<String>,
        auto_select: bool,
    },
    Install(InstallEvent),
    ProviderTested(Result<Vec<String>, String>),
    #[allow(clippy::type_complexity)]
    OnboardingData(
        Box<(
            astrodev_hardware::HardwareInfo,
            astrodev_hardware::Catalog,
            astrodev_installer::runtime::RuntimeStatus,
            Vec<String>,
        )>,
    ),
    InstallDone(Box<Result<InstallOutcome, String>>),
    Installed(Vec<InstalledModel>),
    RefreshInstalled,
    /// Hardware detectado y catálogo cargado para `/hardware`.
    Hardware(Box<(astrodev_hardware::HardwareInfo, astrodev_hardware::Catalog)>),
    /// Estado del agente tras una operación (contexto, coste…).
    Stats {
        context_tokens: usize,
        context_window: u32,
        cost: f64,
        usage: Usage,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Ok { summary: String, diff: Option<String> },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Welcome,
    User(String),
    Assistant { text: String, thinking: String },
    Tool { call: ToolCall, status: ToolStatus },
    Info(String),
    Error(String),
}

/// Ventanas modales sobre el chat.
pub enum Modal {
    Permission {
        req: PermissionRequest,
        scroll: u16,
        /// `Some` mientras el usuario escribe por qué deniega.
        feedback: Option<Input>,
    },
    Sessions {
        items: Vec<SessionMeta>,
        selected: usize,
    },
    Hardware(HardwareView),
    Install(InstallView),
    Provider(ProviderWizard),
    Onboarding(Onboarding),
}

pub struct App {
    pub entries: Vec<Entry>,
    pub input: Input,
    /// Líneas desplazadas desde el final (0 = pegado abajo).
    pub scroll_from_bottom: u16,
    pub busy: bool,
    pub cancel: Option<CancellationToken>,
    pub agent: Arc<Mutex<Agent>>,
    pub provider_name: String,
    pub model: String,
    pub project: String,
    pub project_root: PathBuf,
    pub has_astro_md: bool,
    pub usage: Usage,
    pub cost: f64,
    pub context_tokens: usize,
    pub context_window: u32,
    pub perm_mode: PermissionMode,
    pub last_tps: Option<f64>,
    pub spinner: usize,
    pub should_quit: bool,
    pub tool_names: Vec<String>,
    pub modal: Option<Modal>,
    pub config: Config,
    pub config_path: PathBuf,
    pub factory: ProviderFactory,
    pub tx: mpsc::UnboundedSender<AppMsg>,
    /// Asistente de bienvenida en pausa mientras se instala o se añade un proveedor.
    pub onboarding_pending: Option<Onboarding>,
    pub last_install: Option<Result<String, String>>,
    /// Archivo a abrir en el editor externo (el bucle principal suspende la TUI).
    pub pending_editor: Option<PathBuf>,
    /// Memoria cargada: (tipo, ruta para mostrar).
    pub memory: Vec<(String, String)>,
}

impl App {
    pub async fn new(
        agent: Arc<Mutex<Agent>>,
        config: Config,
        config_path: PathBuf,
        factory: ProviderFactory,
        tx: mpsc::UnboundedSender<AppMsg>,
    ) -> Self {
        let a = agent.lock().await;
        let root = a.project_root().to_path_buf();
        let mut app = Self {
            entries: vec![Entry::Welcome],
            input: Input::default(),
            scroll_from_bottom: 0,
            busy: false,
            cancel: None,
            provider_name: a.provider_name().to_string(),
            model: a.model().to_string(),
            project: root.display().to_string(),
            has_astro_md: root.join(astrodev_core::project::CONTEXT_FILE).exists(),
            project_root: root,
            usage: a.usage(),
            cost: a.cost_usd(),
            context_tokens: a.context_tokens(),
            context_window: a.context_window(),
            perm_mode: a.permission_mode(),
            last_tps: None,
            spinner: 0,
            should_quit: false,
            tool_names: a.tool_names(),
            modal: None,
            config,
            config_path,
            factory,
            tx,
            onboarding_pending: None,
            last_install: None,
            pending_editor: None,
            memory: Vec::new(),
            agent: agent.clone(),
        };
        if !a.messages().is_empty() {
            app.entries.extend(entries_from_messages(a.messages()));
            app.entries.push(Entry::Info(format!("Sesión reanudada: {}", a.session().title)));
        }
        drop(a);
        app.refresh_memory();
        app
    }

    /// Comprueba el proveedor al arrancar y, si hace falta, elige modelo.
    pub fn startup_checks(&mut self, _auto_select: bool) {
        // Siempre en modo "comprobar": elige modelo si falta y sólo avisa si algo va mal.
        self.fetch_models(true);
    }

    fn fetch_models(&self, auto_select: bool) {
        let agent = self.agent.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let provider = agent.lock().await.provider();
            match provider.list_models().await {
                Ok(models) => {
                    let _ = tx.send(AppMsg::Models { models: models.into_iter().map(|m| m.id).collect(), auto_select });
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Error(format!(
                        "{e}\nConsejo: /hardware para ver qué modelo te cabe e /install para instalarlo, o /provider add para usar una API."
                    )));
                }
            }
        });
    }

    /// Pide al agente sus estadísticas (en segundo plano, cuando quede libre).
    pub fn refresh_stats(&self) {
        let agent = self.agent.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let a = agent.lock().await;
            let _ = tx.send(AppMsg::Stats {
                context_tokens: a.context_tokens(),
                context_window: a.context_window(),
                cost: a.cost_usd(),
                usage: a.usage(),
            });
        });
    }

    pub fn tick(&mut self) {
        if self.busy
            || matches!(&self.modal, Some(Modal::Install(v)) if !v.finished())
            || matches!(&self.modal, Some(Modal::Provider(w)) if matches!(w.step, crate::provider_view::Step::Testing))
            || matches!(&self.modal, Some(Modal::Onboarding(o)) if o.hw.is_none())
        {
            self.spinner = self.spinner.wrapping_add(1);
        }
    }

    pub fn on_terminal_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if self.modal.is_some() {
                    self.on_modal_key(k);
                } else {
                    self.on_key(k);
                }
            }
            Event::Paste(s) => match &mut self.modal {
                Some(Modal::Permission { feedback: Some(i), .. }) => i.insert_str(&s),
                Some(Modal::Provider(w)) => w.paste(&s),
                None => self.input.insert_str(&s),
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => self.scroll_up(3),
                MouseEventKind::ScrollDown => self.scroll_down(3),
                _ => {}
            },
            _ => {}
        }
    }

    fn scroll_up(&mut self, n: u16) {
        match &mut self.modal {
            Some(Modal::Permission { scroll, .. }) => *scroll = scroll.saturating_sub(n),
            _ => self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(n),
        }
    }

    fn scroll_down(&mut self, n: u16) {
        match &mut self.modal {
            Some(Modal::Permission { scroll, .. }) => *scroll = scroll.saturating_add(n),
            _ => self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n),
        }
    }

    fn on_modal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            // Ctrl+C dentro de un modal: denegar/cerrar y cancelar el turno.
            if let Some(Modal::Permission { req, .. }) = self.modal.take() {
                req.respond(PermissionReply::Deny { feedback: None });
            }
            self.cancel_turn();
            return;
        }
        let Some(modal) = self.modal.as_mut() else {
            return;
        };
        match modal {
            Modal::Permission { req, scroll, feedback } => {
                if let Some(input) = feedback {
                    match k.code {
                        KeyCode::Enter => {
                            let text = input.text().trim().to_string();
                            req.respond(PermissionReply::Deny { feedback: (!text.is_empty()).then_some(text) });
                            self.modal = None;
                        }
                        KeyCode::Esc => *feedback = None,
                        KeyCode::Backspace => input.backspace(),
                        KeyCode::Left => input.left(),
                        KeyCode::Right => input.right(),
                        KeyCode::Char(c) => input.insert_char(c),
                        _ => {}
                    }
                    return;
                }
                let reply = match k.code {
                    KeyCode::Char('s') | KeyCode::Char('y') | KeyCode::Char('1') | KeyCode::Enter => {
                        Some(PermissionReply::AllowOnce)
                    }
                    KeyCode::Char('a') | KeyCode::Char('2') if !req.dangerous => Some(PermissionReply::AllowSession),
                    KeyCode::Char('n') | KeyCode::Char('3') | KeyCode::Esc => {
                        Some(PermissionReply::Deny { feedback: None })
                    }
                    KeyCode::Char('d') | KeyCode::Char('4') => {
                        *feedback = Some(Input::default());
                        None
                    }
                    KeyCode::Up => {
                        *scroll = scroll.saturating_sub(1);
                        None
                    }
                    KeyCode::Down => {
                        *scroll = scroll.saturating_add(1);
                        None
                    }
                    KeyCode::PageUp => {
                        *scroll = scroll.saturating_sub(10);
                        None
                    }
                    KeyCode::PageDown => {
                        *scroll = scroll.saturating_add(10);
                        None
                    }
                    _ => None,
                };
                if let Some(r) = reply {
                    if r == PermissionReply::AllowSession {
                        self.entries.push(Entry::Info(format!(
                            "`{}` permitido sin preguntar durante esta sesión.",
                            req.call.name
                        )));
                    }
                    req.respond(r);
                    self.modal = None;
                }
            }
            Modal::Hardware(v) => match v.on_key(k) {
                HwAction::None => {}
                HwAction::Close => self.modal = None,
                HwAction::Install { rec, ctx, llamacpp } => {
                    self.modal = None;
                    self.start_install(InstallRequest {
                        model_id: rec.model_id,
                        quant: Some(rec.quant),
                        runtime: if llamacpp { RuntimeKind::LlamaCpp } else { RuntimeKind::Ollama },
                        ctx: Some(ctx),
                        force: false,
                    });
                }
                HwAction::InstallTag(tag) => {
                    self.modal = None;
                    self.start_install_tag(tag);
                }
                HwAction::Use(m) => {
                    self.modal = None;
                    self.use_installed(m);
                }
                HwAction::Delete(m) => {
                    let name = m.name.clone();
                    self.entries.push(Entry::Info(format!("Borrando {name}…")));
                    let tx = self.tx.clone();
                    let url = self.ollama_url();
                    tokio::spawn(async move {
                        let r = match m.source {
                            Source::Gguf => astrodev_installer::llamacpp::delete_model(&m.name)
                                .map(|_| format!("Borrado {}", m.name)),
                            Source::Ollama => astrodev_installer::pipeline::uninstall_model(&m.name, &url).await,
                        };
                        let _ = tx.send(match r {
                            Ok(s) => AppMsg::Info(s),
                            Err(e) => AppMsg::Error(e.to_string()),
                        });
                        let _ = tx.send(AppMsg::RefreshInstalled);
                    });
                }
            },
            Modal::Provider(w) => match w.on_key(k) {
                WizardAction::None => {}
                WizardAction::Close => {
                    self.modal = None;
                    self.resume_onboarding(None, false, true);
                }
                WizardAction::Test => self.test_new_provider(),
                WizardAction::Finish(model) => {
                    self.finish_new_provider(model.clone());
                    self.resume_onboarding(
                        Some(format!("API configurada: {}/{model}", self.provider_name)),
                        false,
                        false,
                    );
                }
            },
            Modal::Onboarding(o) => match o.on_key(k) {
                OnboardAction::None => {}
                OnboardAction::Skip => self.finish_onboarding(false, true),
                OnboardAction::InstallModel(r) => {
                    if let Some(Modal::Onboarding(o)) = self.modal.take() {
                        self.onboarding_pending = Some(o);
                    }
                    self.start_install(InstallRequest {
                        model_id: r.model_id,
                        quant: Some(r.quant),
                        runtime: RuntimeKind::Ollama,
                        ctx: None,
                        force: false,
                    });
                }
                OnboardAction::UseInstalledModel(m) => {
                    if let Some(Modal::Onboarding(o)) = self.modal.take() {
                        self.onboarding_pending = Some(o);
                    }
                    let p = self.ollama_provider_name();
                    self.switch_model(&format!("{p}/{m}"));
                    self.config.default_provider = p;
                    self.config.default_model = Some(m.clone());
                    self.resume_onboarding(Some(format!("Modelo local: {m}")), true, false);
                }
                OnboardAction::AddProvider => {
                    if let Some(Modal::Onboarding(o)) = self.modal.take() {
                        self.onboarding_pending = Some(o);
                    }
                    self.modal = Some(Modal::Provider(ProviderWizard::new()));
                }
                OnboardAction::Finish { init_astro_md } => self.finish_onboarding(init_astro_md, false),
            },
            Modal::Install(v) => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    if v.finished() {
                        self.modal = None;
                        if self.onboarding_pending.is_some() {
                            let note = match self.last_install.take() {
                                Some(Ok(s)) => Some(s),
                                _ => None,
                            };
                            let ok = note.is_some();
                            // Si falló, volver a la lista para elegir otro modelo.
                            self.resume_onboarding(note, ok, false);
                        }
                    } else if k.code == KeyCode::Esc {
                        v.cancel.cancel();
                    }
                }
                _ => {}
            },
            Modal::Sessions { items, selected } => match k.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(items.len().saturating_sub(1)),
                KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                KeyCode::Enter => {
                    let id = items.get(*selected).map(|m| m.id.clone());
                    self.modal = None;
                    if let Some(id) = id {
                        self.resume(&id);
                    }
                }
                _ => {}
            },
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy {
                    self.cancel_turn();
                } else if !self.input.is_empty() {
                    self.input.clear();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.input.is_empty() {
                    self.should_quit = true;
                }
            }
            KeyCode::Esc => {
                if self.busy {
                    self.cancel_turn();
                }
            }
            KeyCode::BackTab => self.cycle_mode(),
            KeyCode::Tab => {
                if let Some(c) = commands::complete(self.input.text()) {
                    self.input.clear();
                    self.input.insert_str(&format!("/{c} "));
                }
            }
            KeyCode::Char('j') if ctrl => self.input.insert_char('\n'),
            KeyCode::Enter if alt || shift => self.input.insert_char('\n'),
            KeyCode::Enter => self.submit(),
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char('w') if ctrl => self.input.delete_word(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char(c) if !ctrl => self.input.insert_char(c),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up if !self.input.is_multiline() => self.input.history_prev(),
            KeyCode::Down if !self.input.is_multiline() => self.input.history_next(),
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            _ => {}
        }
    }

    fn cancel_turn(&mut self) {
        if let Some(c) = &self.cancel {
            c.cancel();
        }
    }

    fn set_mode(&mut self, mode: PermissionMode) {
        self.perm_mode = mode;
        let agent = self.agent.clone();
        tokio::spawn(async move {
            agent.lock().await.permissions_mut().mode = mode;
        });
        let desc = match mode {
            PermissionMode::Ask => "ask — pregunta antes de editar archivos o ejecutar comandos",
            PermissionMode::AutoProject => {
                "auto-project — ediciones dentro del proyecto automáticas; comandos preguntan"
            }
            PermissionMode::AutoAll => "auto-all — todo automático (los comandos peligrosos siguen preguntando)",
        };
        self.entries.push(Entry::Info(format!("Modo de permisos: {desc}")));
    }

    fn cycle_mode(&mut self) {
        let next = match self.perm_mode {
            PermissionMode::Ask => PermissionMode::AutoProject,
            PermissionMode::AutoProject => PermissionMode::AutoAll,
            PermissionMode::AutoAll => PermissionMode::Ask,
        };
        self.set_mode(next);
    }

    fn submit(&mut self) {
        if self.input.text().trim().is_empty() {
            return;
        }
        let text = self.input.take();
        self.scroll_from_bottom = 0;
        // Atajo de memoria: "# nota" se guarda en el ASTRO.md del proyecto.
        if let Some(note) = text.strip_prefix('#').filter(|n| n.starts_with(' ') && !n.trim().is_empty()) {
            self.quick_note(note.trim());
            return;
        }
        if let Some(cmd) = commands::parse(&text) {
            self.run_command(cmd);
            return;
        }
        self.send_prompt(text.clone(), text);
    }

    /// Envía un turno al agente. `shown` es lo que se ve en el chat.
    pub fn send_prompt(&mut self, shown: String, prompt: String) {
        if self.busy {
            self.entries.push(Entry::Error("Espera a que termine la respuesta actual (Esc para cancelar).".into()));
            return;
        }
        if self.model.is_empty() {
            self.entries.push(Entry::Error("No hay modelo seleccionado. Usa /model, /install o /provider add.".into()));
            return;
        }
        self.entries.push(Entry::User(shown));
        self.busy = true;
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        let agent = self.agent.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let (atx, mut arx) = mpsc::channel(256);
            let fwd_tx = tx.clone();
            let fwd = tokio::spawn(async move {
                while let Some(e) = arx.recv().await {
                    let _ = fwd_tx.send(AppMsg::Agent(e));
                }
            });
            {
                let mut a = agent.lock().await;
                a.run_turn(prompt, atx, cancel).await;
                let _ = tx.send(AppMsg::Stats {
                    context_tokens: a.context_tokens(),
                    context_window: a.context_window(),
                    cost: a.cost_usd(),
                    usage: a.usage(),
                });
            }
            let _ = fwd.await;
            let _ = tx.send(AppMsg::TurnFinished);
        });
    }

    fn require_idle(&mut self) -> bool {
        if self.busy {
            self.entries.push(Entry::Error("Espera a que termine la respuesta actual.".into()));
            return false;
        }
        true
    }

    fn resume(&mut self, id: &str) {
        let Ok(mut a) = self.agent.try_lock() else {
            self.entries.push(Entry::Error("El agente está ocupado.".into()));
            return;
        };
        let Some(store) = a.store() else {
            self.entries.push(Entry::Error("No hay almacén de sesiones configurado.".into()));
            return;
        };
        match store.load(id) {
            Ok(s) => {
                let title = s.title.clone();
                a.load_session(s);
                self.model = a.model().to_string();
                self.entries = vec![Entry::Welcome];
                self.entries.extend(entries_from_messages(a.messages()));
                self.entries.push(Entry::Info(format!("Sesión reanudada: {title}")));
                self.usage = a.usage();
                self.cost = a.cost_usd();
                self.context_tokens = a.context_tokens();
            }
            Err(e) => self.entries.push(Entry::Error(format!("No se pudo cargar la sesión: {e}"))),
        }
    }

    pub fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::Help => self.entries.push(Entry::Info(commands::HELP.into())),
            Command::Quit => self.should_quit = true,
            Command::Clear => {
                if !self.require_idle() {
                    return;
                }
                if let Ok(mut a) = self.agent.try_lock() {
                    a.new_session();
                }
                self.entries = vec![Entry::Welcome, Entry::Info("Nueva conversación.".into())];
                self.usage = Usage::default();
                self.cost = 0.0;
                self.refresh_stats();
            }
            Command::Model(None) => {
                self.entries.push(Entry::Info(format!("Consultando modelos de {}…", self.provider_name)));
                self.fetch_models(false);
            }
            Command::Model(Some(m)) => {
                if !self.require_idle() {
                    return;
                }
                self.switch_model(&m);
            }
            Command::Tools => self.entries.push(Entry::Info(format!(
                "Herramientas: {}\nLas de lectura se ejecutan solas; las que editan o ejecutan comandos piden permiso según el modo ({}).",
                self.tool_names.join(", "),
                mode_name(self.perm_mode)
            ))),
            Command::Cost => self.entries.push(Entry::Info(format!(
                "Tokens: {} entrada · {} salida · coste estimado ${:.4}{}",
                self.usage.input_tokens,
                self.usage.output_tokens,
                self.cost,
                self.last_tps.map(|t| format!(" · última velocidad {t:.1} tok/s")).unwrap_or_default()
            ))),
            Command::Context => self.entries.push(Entry::Info(format!(
                "Contexto: ~{} de {} tokens ({}%). Se resume automáticamente al acercarse al límite; fuerza con /compact.",
                self.context_tokens,
                self.context_window,
                self.context_pct()
            ))),
            Command::Compact => {
                if !self.require_idle() {
                    return;
                }
                self.busy = true;
                let agent = self.agent.clone();
                let tx = self.tx.clone();
                self.entries.push(Entry::Info("Resumiendo la conversación…".into()));
                tokio::spawn(async move {
                    let (atx, mut arx) = mpsc::channel(16);
                    let mut a = agent.lock().await;
                    a.maybe_compact(true, &atx, &CancellationToken::new()).await;
                    a.save();
                    drop(atx);
                    while let Some(e) = arx.recv().await {
                        let _ = tx.send(AppMsg::Agent(e));
                    }
                    let _ = tx.send(AppMsg::Stats {
                        context_tokens: a.context_tokens(),
                        context_window: a.context_window(),
                        cost: a.cost_usd(),
                        usage: a.usage(),
                    });
                    let _ = tx.send(AppMsg::TurnFinished);
                });
            }
            Command::Undo => {
                if !self.require_idle() {
                    return;
                }
                let Ok(mut a) = self.agent.try_lock() else { return };
                match a.undo() {
                    Some(rep) => {
                        let mut s = format!("Deshecho el turno «{}».", rep.label.lines().next().unwrap_or(""));
                        let root = a.project_root().to_path_buf();
                        let rel = |p: &PathBuf| p.strip_prefix(&root).unwrap_or(p).display().to_string();
                        if !rep.restored.is_empty() {
                            s.push_str(&format!("\n  restaurados: {}", rep.restored.iter().map(rel).collect::<Vec<_>>().join(", ")));
                        }
                        if !rep.deleted.is_empty() {
                            s.push_str(&format!("\n  eliminados: {}", rep.deleted.iter().map(rel).collect::<Vec<_>>().join(", ")));
                        }
                        for (p, e) in &rep.failed {
                            s.push_str(&format!("\n  ✗ {}: {e}", rel(p)));
                        }
                        s.push_str("\n  (los efectos de comandos de shell no se pueden deshacer)");
                        self.entries = vec![Entry::Welcome];
                        self.entries.extend(entries_from_messages(a.messages()));
                        self.entries.push(Entry::Info(s));
                        // Devolver el texto del turno a la entrada para editarlo.
                        self.input.clear();
                        self.input.insert_str(&rep.label);
                    }
                    None => self.entries.push(Entry::Info("No hay nada que deshacer.".into())),
                }
                drop(a);
                self.refresh_stats();
            }
            Command::Resume(Some(id)) => {
                if self.require_idle() {
                    self.resume(&id);
                }
            }
            Command::Resume(None) => {
                if !self.require_idle() {
                    return;
                }
                let items = self
                    .agent
                    .try_lock()
                    .ok()
                    .and_then(|a| a.store().map(|s| s.list(Some(a.project_root()))))
                    .unwrap_or_default();
                if items.is_empty() {
                    self.entries.push(Entry::Info("No hay sesiones guardadas para este proyecto.".into()));
                } else {
                    self.modal = Some(Modal::Sessions { items, selected: 0 });
                }
            }
            Command::Permissions(None) => self.entries.push(Entry::Info(format!(
                "Modo actual: {}. Cambia con /permissions ask|auto-project|auto-all o Shift+Tab.",
                mode_name(self.perm_mode)
            ))),
            Command::Permissions(Some(m)) => match m.as_str() {
                "ask" | "preguntar" => self.set_mode(PermissionMode::Ask),
                "auto-project" | "auto-proyecto" | "project" => self.set_mode(PermissionMode::AutoProject),
                "auto-all" | "auto" | "all" => self.set_mode(PermissionMode::AutoAll),
                other => self.entries.push(Entry::Error(format!("Modo desconocido `{other}`. Usa ask, auto-project o auto-all."))),
            },
            Command::Init => {
                let prompt = "Analiza este proyecto y crea (o mejora, sin perder lo que ya haya) el archivo ASTRO.md de la raíz: \
                    la memoria que AstroDev lee en cada conversación, como CLAUDE.md. Explora primero (list_dir, \
                    manifiestos como Cargo.toml/package.json/pyproject.toml, README, CI, configuración de linters y tests). \
                    Escribe en Markdown, conciso (40-80 líneas), con estas secciones si aplican:\n\
                    # <nombre del proyecto>\n\
                    ## Qué es — 1-3 frases.\n\
                    ## Comandos — compilar, ejecutar, tests (y cómo correr uno solo), lint/formato, en bloques de código.\n\
                    ## Arquitectura — carpetas y módulos clave, y cómo se relacionan.\n\
                    ## Convenciones — estilo de código, nombres, manejo de errores, tests, commits.\n\
                    ## Flujo de trabajo — qué verificar antes de dar algo por terminado.\n\
                    ## Cuidado con — trampas conocidas, archivos generados que no se editan, secretos.\n\
                    Sólo información específica y verificable de ESTE proyecto: nada genérico ni inventado."
                    .to_string();
                self.send_prompt("/init — generar ASTRO.md".into(), prompt);
            }
            Command::Hardware => self.open_hardware(),
            Command::Install(None) => self.open_hardware(),
            Command::Install(Some(arg)) => self.install_command(&arg),
            Command::Uninstall(None) => self.entries.push(Entry::Info(
                "Uso: /uninstall <modelo> (de Ollama o un .gguf) · /uninstall ollama|llamacpp para el runtime".into(),
            )),
            Command::Uninstall(Some(name)) => self.uninstall(name),
            Command::Setup => self.start_onboarding(),
            Command::Theme(None) => self.entries.push(Entry::Info(format!(
                "Tema actual: {}. Disponibles: mono (blanco y negro) · astro (a color). Cambia con /theme <nombre>.",
                crate::theme::th().name
            ))),
            Command::Theme(Some(t)) => {
                if crate::theme::set_theme(&t) {
                    self.config.ui.theme = crate::theme::th().name.to_string();
                    let _ = self.config.save(&self.config_path);
                    self.entries.push(Entry::Info(format!("Tema: {}", crate::theme::th().name)));
                } else {
                    self.entries.push(Entry::Error(format!("Tema desconocido `{t}`. Usa mono o astro.")));
                }
            }
            Command::Memory(arg) => self.memory_command(arg.as_deref()),
            Command::Provider(arg) => self.provider_command(arg.as_deref().unwrap_or("list")),
            Command::Unknown(n) => self.entries.push(Entry::Error(format!("Comando desconocido /{n}. Escribe /help."))),
        }
    }

    /// Abre `/hardware` y detecta en segundo plano.
    pub fn open_hardware(&mut self) {
        self.modal = Some(Modal::Hardware(HardwareView::loading(self.model.clone())));
        self.fetch_installed();
        let tx = self.tx.clone();
        let catalog_url = self.config.catalog_url.clone();
        tokio::spawn(async move {
            let data = astrodev_core::config::data_dir().unwrap_or_else(|_| std::env::temp_dir());
            if astrodev_hardware::Catalog::cache_is_stale(&data) {
                let url = catalog_url.unwrap_or_else(|| astrodev_hardware::catalog::DEFAULT_URL.to_string());
                if let Err(e) = astrodev_hardware::Catalog::update(&url, &data).await {
                    tracing::info!("catálogo no actualizado: {e}");
                }
            }
            let d2 = data.clone();
            let hw = tokio::task::spawn_blocking(move || astrodev_hardware::detect(&d2.join("models"))).await;
            if let Ok(hw) = hw {
                let cat = astrodev_hardware::Catalog::load(&data);
                let _ = tx.send(AppMsg::Hardware(Box::new((hw, cat))));
            }
        });
    }

    /// Modelos descargados: los de Ollama y los GGUF de llama.cpp.
    fn fetch_installed(&self) {
        let tx = self.tx.clone();
        let url = self.ollama_url();
        tokio::spawn(async move {
            use astrodev_core::Provider as _;
            let mut list: Vec<InstalledModel> = Vec::new();
            if let Ok(models) = astrodev_providers::OllamaProvider::new("ollama", url).list_models().await {
                list.extend(models.into_iter().map(|m| InstalledModel {
                    name: m.id,
                    size: m.size_bytes.unwrap_or(0),
                    source: Source::Ollama,
                    fit: None,
                }));
            }
            list.extend(astrodev_installer::llamacpp::list_models().into_iter().map(|(name, size)| InstalledModel {
                name,
                size,
                source: Source::Gguf,
                fit: None,
            }));
            let _ = tx.send(AppMsg::Installed(list));
        });
    }

    /// Usar un modelo ya instalado (desde `/hardware` → Instalados).
    fn use_installed(&mut self, m: InstalledModel) {
        match m.source {
            Source::Ollama => {
                let p = self.ollama_provider_name();
                self.switch_model(&format!("{p}/{}", m.name));
                self.config.default_provider = p;
                self.config.default_model = Some(m.name.clone());
                if let Err(e) = self.config.save(&self.config_path) {
                    self.entries.push(Entry::Error(format!("No se pudo guardar la configuración: {e}")));
                }
            }
            Source::Gguf => self.entries.push(Entry::Info(format!(
                "{} es un GGUF de llama.cpp: arráncalo con `astrodev pull` o `llama-server -m {}` y usa /model llamacpp/<modelo>.",
                m.name,
                astrodev_installer::paths::models_dir().join(&m.name).display()
            ))),
        }
    }

    /// Descarga con Ollama un modelo que no está en el catálogo.
    pub fn start_install_tag(&mut self, tag: String) {
        let cancel = CancellationToken::new();
        self.modal = Some(Modal::Install(InstallView::new(format!("Instalar {tag} con Ollama"), cancel.clone())));
        let tx = self.tx.clone();
        let url = self.ollama_url();
        let maker = self.provider_maker();
        tokio::spawn(async move {
            let (etx, mut erx) = mpsc::unbounded_channel();
            let fwd_tx = tx.clone();
            let fwd = tokio::spawn(async move {
                while let Some(e) = erx.recv().await {
                    let _ = fwd_tx.send(AppMsg::Install(e));
                }
            });
            let rep = Reporter::new(etx);
            let hw =
                tokio::task::spawn_blocking(|| astrodev_hardware::detect(&astrodev_installer::paths::models_dir()))
                    .await
                    .expect("detección");
            let res = astrodev_installer::pipeline::install_ollama_tag(&tag, &hw, &url, &maker, &rep, &cancel).await;
            match &res {
                Ok(o) => rep.send(InstallEvent::Done(o.summary.clone())),
                Err(e) => rep.send(InstallEvent::Error(e.to_string())),
            }
            drop(rep);
            let _ = fwd.await;
            let _ = tx.send(AppMsg::InstallDone(Box::new(res.map_err(|e| e.to_string()))));
        });
    }

    pub fn refresh_memory(&mut self) {
        let Some(p) = self.project() else { return };
        self.memory = astrodev_core::memory::load(&p, p.cwd())
            .iter()
            .map(|f| {
                let shown = if f.kind == astrodev_core::memory::MemoryKind::Global {
                    f.path.display().to_string().replace(&std::env::var("HOME").unwrap_or_default(), "~")
                } else {
                    p.display(&f.path)
                };
                (f.kind.label().to_string(), shown)
            })
            .collect();
        self.has_astro_md = !self.memory.is_empty();
    }

    /// Proyecto actual, conservando el subdirectorio de trabajo (para su ASTRO.md).
    fn project(&self) -> Option<astrodev_core::Project> {
        let start = std::env::current_dir()
            .ok()
            .and_then(|c| astrodev_core::project::dunce_canonicalize(&c).ok())
            .filter(|c| c.starts_with(&self.project_root))
            .unwrap_or_else(|| self.project_root.clone());
        astrodev_core::Project::discover(start).ok()
    }

    pub fn after_editor(&mut self, path: &std::path::Path, res: Result<(), String>) {
        match res {
            Ok(()) => {
                self.refresh_memory();
                self.entries
                    .push(Entry::Info(format!("Guardado {}. Se aplica desde el próximo mensaje.", path.display())));
                self.refresh_stats();
            }
            Err(e) => self.entries.push(Entry::Error(e)),
        }
    }

    fn quick_note(&mut self, note: &str) {
        let Some(p) = self.project() else { return };
        let path = astrodev_core::memory::project_path(&p);
        match astrodev_core::memory::append_note(&path, note) {
            Ok(()) => {
                self.refresh_memory();
                self.entries.push(Entry::Info(format!(
                    "Recordado en {}: «{note}». Se aplica desde el próximo mensaje.",
                    p.display(&path)
                )));
            }
            Err(e) => self.entries.push(Entry::Error(format!("No se pudo escribir {}: {e}", path.display()))),
        }
    }

    fn memory_command(&mut self, arg: Option<&str>) {
        use astrodev_core::memory::{self, MemoryKind};
        let Some(p) = self.project() else { return };
        let mut parts = arg.unwrap_or("").split_whitespace();
        match parts.next() {
            Some("edit") | Some("editar") => {
                let which = parts.next().unwrap_or("proyecto");
                let path = match which {
                    "global" => match memory::global_path() {
                        Some(g) => g,
                        None => return,
                    },
                    "local" | "personal" => {
                        if let Ok(true) = memory::ignore_local(&p) {
                            self.entries.push(Entry::Info(format!("Añadido {} a .gitignore.", memory::LOCAL_FILE)));
                        }
                        memory::local_path(&p)
                    }
                    _ => memory::project_path(&p),
                };
                if !path.exists() {
                    let title = match which {
                        "global" => "# Preferencias globales\n\n",
                        "local" | "personal" => "# Notas personales (no se suben a git)\n\n",
                        _ => "# Proyecto\n\n",
                    };
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(&path, title);
                }
                self.pending_editor = Some(path);
            }
            _ => {
                let files = memory::load(&p, p.cwd());
                let mut s = String::from("Memoria cargada en cada conversación (lo más específico prevalece):\n");
                if files.is_empty() {
                    s.push_str("  (ninguna)\n");
                }
                for f in &files {
                    let shown =
                        if f.kind == MemoryKind::Global { f.path.display().to_string() } else { p.display(&f.path) };
                    let n = f.content.lines().count();
                    s.push_str(&format!(
                        "  • {:<13} {} · {n} {}\n",
                        f.kind.label(),
                        shown,
                        if n == 1 { "línea" } else { "líneas" }
                    ));
                    for i in &f.imports {
                        s.push_str(&format!("      ↳ importa {}\n", i.display()));
                    }
                }
                s.push_str(
                    "\n# texto → nota rápida en el ASTRO.md del proyecto · /init lo genera analizando el código\n\
                     /memory edit proyecto|local|global abre el archivo en tu editor ($EDITOR) · @ruta importa otro archivo",
                );
                self.entries.push(Entry::Info(s));
            }
        }
    }

    fn ollama_provider_name(&self) -> String {
        self.config
            .providers
            .iter()
            .find(|(_, p)| p.kind == astrodev_core::config::ProviderKind::Ollama)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "ollama".into())
    }

    /// Abre el asistente de bienvenida y detecta el hardware en segundo plano.
    pub fn start_onboarding(&mut self) {
        let has_md = self.project_root.join(astrodev_core::project::CONTEXT_FILE).exists();
        self.modal = Some(Modal::Onboarding(Onboarding::new(self.project.clone(), has_md)));
        let tx = self.tx.clone();
        let url = self.ollama_url();
        let catalog_url = self.config.catalog_url.clone();
        tokio::spawn(async move {
            let data = astrodev_installer::paths::data_dir();
            if astrodev_hardware::Catalog::cache_is_stale(&data) {
                let u = catalog_url.unwrap_or_else(|| astrodev_hardware::catalog::DEFAULT_URL.to_string());
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    astrodev_hardware::Catalog::update(&u, &data),
                )
                .await;
            }
            let hw =
                tokio::task::spawn_blocking(|| astrodev_hardware::detect(&astrodev_installer::paths::models_dir()))
                    .await
                    .expect("detección");
            let catalog = astrodev_hardware::Catalog::load(&data);
            let st = astrodev_installer::runtime::ollama_status(&url).await;
            let models = if st.running {
                let p = astrodev_providers::OllamaProvider::new("ollama", url.clone());
                use astrodev_core::Provider as _;
                p.list_models().await.map(|m| m.into_iter().map(|m| m.id).collect()).unwrap_or_default()
            } else {
                Vec::new()
            };
            let _ = tx.send(AppMsg::OnboardingData(Box::new((hw, catalog, st, models))));
        });
    }

    /// Vuelve al asistente tras instalar o añadir un proveedor.
    /// `after_local`: se completó el paso local. `cancelled`: el usuario cerró un sub-asistente.
    fn resume_onboarding(&mut self, note: Option<String>, after_local: bool, cancelled: bool) {
        let Some(mut o) = self.onboarding_pending.take() else { return };
        if let Some(n) = note {
            o.done.push(n);
        }
        if after_local {
            if let OnboardAction::AddProvider = o.after_local() {
                self.onboarding_pending = Some(o);
                self.modal = Some(Modal::Provider(ProviderWizard::new()));
                return;
            }
        } else if o.mode == Some(crate::onboarding_view::Mode::Api) {
            // Vuelta del asistente de API: completado → proyecto; cancelado → elegir modo.
            o.step = if cancelled && o.done.is_empty() {
                crate::onboarding_view::Step::Mode
            } else {
                crate::onboarding_view::Step::Project
            };
        }
        self.modal = Some(Modal::Onboarding(o));
    }

    fn finish_onboarding(&mut self, init_astro_md: bool, skipped: bool) {
        self.modal = None;
        self.onboarding_pending = None;
        if let Err(e) = self.config.save(&self.config_path) {
            self.entries.push(Entry::Error(format!("No se pudo guardar la configuración: {e}")));
        }
        if skipped {
            self.entries.push(Entry::Info(format!(
                "Asistente omitido. Configuración en {}. Puedes reabrirlo con /setup.",
                self.config_path.display()
            )));
        } else {
            self.entries.push(Entry::Info("¡Listo! Escribe qué quieres hacer en este proyecto.".into()));
        }
        self.startup_checks(self.model.is_empty());
        if init_astro_md {
            self.run_command(Command::Init);
        }
    }

    fn provider_command(&mut self, arg: &str) {
        let mut parts = arg.split_whitespace();
        match parts.next().unwrap_or("list") {
            "add" | "nuevo" => self.modal = Some(Modal::Provider(ProviderWizard::new())),
            "remove" | "rm" => {
                let Some(name) = parts.next() else {
                    self.entries.push(Entry::Error("Uso: /provider remove <nombre>".into()));
                    return;
                };
                match self.config.providers.remove(name) {
                    Some(p) => {
                        if let Some(r) = p.api_key_ref {
                            tokio::task::spawn_blocking(move || astrodev_providers::keys::keyring_delete(&r));
                        }
                        let _ = self.config.save(&self.config_path);
                        self.entries.push(Entry::Info(format!("Proveedor {name} eliminado (y su clave del keyring).")));
                    }
                    None => self.entries.push(Entry::Error(format!("No existe el proveedor {name}."))),
                }
            }
            _ => {
                let mut s = String::from("Proveedores (● = en uso):\n");
                for (name, p) in &self.config.providers {
                    let mark = if *name == self.provider_name { "●" } else { " " };
                    s.push_str(&format!(
                        "  {mark} {name:<12} {:<9} {}{}\n",
                        format!("{:?}", p.kind).to_lowercase(),
                        p.base_url.clone().unwrap_or_default(),
                        p.default_model.as_ref().map(|m| format!("  ({m})")).unwrap_or_default()
                    ));
                }
                s.push_str(
                    "/provider add para añadir · /model <proveedor>/<modelo> para cambiar · /provider remove <nombre>",
                );
                self.entries.push(Entry::Info(s));
            }
        }
    }

    /// Paso de prueba del asistente: guarda la clave (si hay) y lista modelos.
    fn test_new_provider(&mut self) {
        let Some(Modal::Provider(w)) = &self.modal else { return };
        let Some(preset) = w.preset else { return };
        let name = w.name.text().trim().to_string();
        let url = w.url.text().trim().to_string();
        let key = w.key.text().trim().to_string();
        let tx = self.tx.clone();
        let mut cfg = self.config.clone();
        let factory = self.factory.clone();
        tokio::spawn(async move {
            let mut key_ref = None;
            if !key.is_empty() {
                let (n, k) = (name.clone(), key.clone());
                match tokio::task::spawn_blocking(move || astrodev_providers::keys::keyring_set(&n, &k)).await {
                    Ok(Ok(())) => key_ref = Some(name.clone()),
                    Ok(Err(e)) => {
                        let _ = tx.send(AppMsg::ProviderTested(Err(e)));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::ProviderTested(Err(e.to_string())));
                        return;
                    }
                }
            }
            cfg.providers.insert(name.clone(), preset.config(&url, key_ref));
            let res = match factory(&cfg, &name) {
                Ok(p) => {
                    p.list_models().await.map(|m| m.into_iter().map(|m| m.id).collect()).map_err(|e| e.to_string())
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(AppMsg::ProviderTested(res));
        });
    }

    fn finish_new_provider(&mut self, model: String) {
        let Some(Modal::Provider(w)) = self.modal.take() else { return };
        let Some(preset) = w.preset else { return };
        let name = w.name.text().trim().to_string();
        let key_ref = (!w.key.text().trim().is_empty()).then(|| name.clone());
        let mut pcfg = preset.config(w.url.text().trim(), key_ref);
        pcfg.default_model = Some(model.clone());
        self.config.providers.insert(name.clone(), pcfg);
        self.config.default_provider = name.clone();
        self.config.default_model = Some(model.clone());
        if let Err(e) = self.config.save(&self.config_path) {
            self.entries.push(Entry::Error(format!("No se pudo guardar la configuración: {e}")));
        }
        self.switch_model(&format!("{name}/{model}"));
        self.entries.push(Entry::Info(format!(
            "✓ Proveedor {name} añadido y en uso. La clave está en el keyring del sistema."
        )));
    }

    fn ollama_url(&self) -> String {
        self.config
            .providers
            .values()
            .find(|p| p.kind == astrodev_core::config::ProviderKind::Ollama)
            .and_then(|p| p.base_url.clone())
            .unwrap_or_else(|| astrodev_installer::ollama::DEFAULT_URL.to_string())
    }

    fn provider_maker(&self) -> ProviderMaker {
        let factory = self.factory.clone();
        Arc::new(move |name: &str, pcfg: &astrodev_core::config::ProviderConfig| {
            let mut c = Config::default();
            c.providers.insert(name.to_string(), pcfg.clone());
            factory(&c, name)
        })
    }

    /// `/install <modelo> [ollama|llamacpp]` o `/install ollama|llamacpp`.
    fn install_command(&mut self, arg: &str) {
        let mut parts = arg.split_whitespace();
        let first = parts.next().unwrap_or("");
        if let Some(rt) = RuntimeKind::parse(first) {
            self.start_runtime_install(rt);
            return;
        }
        let runtime = parts.next().and_then(RuntimeKind::parse).unwrap_or(RuntimeKind::Ollama);
        let catalog = astrodev_hardware::Catalog::load(&astrodev_installer::paths::data_dir());
        if catalog.find(first).is_none() {
            // Fuera del catálogo: etiqueta de Ollama o GGUF de Hugging Face.
            self.start_install_tag(first.to_string());
            return;
        }
        self.start_install(InstallRequest {
            model_id: first.to_string(),
            quant: None,
            runtime,
            ctx: None,
            force: false,
        });
    }

    fn start_runtime_install(&mut self, rt: RuntimeKind) {
        let cancel = CancellationToken::new();
        self.modal = Some(Modal::Install(InstallView::new(format!("Instalar {}", rt.label()), cancel.clone())));
        let tx = self.tx.clone();
        let url = self.ollama_url();
        tokio::spawn(async move {
            let (etx, mut erx) = mpsc::unbounded_channel();
            let fwd_tx = tx.clone();
            let fwd = tokio::spawn(async move {
                while let Some(e) = erx.recv().await {
                    let _ = fwd_tx.send(AppMsg::Install(e));
                }
            });
            let rep = Reporter::new(etx);
            let hw =
                tokio::task::spawn_blocking(|| astrodev_hardware::detect(&astrodev_installer::paths::models_dir()))
                    .await
                    .expect("detección");
            let res = match rt {
                RuntimeKind::Ollama => astrodev_installer::ollama::install(&hw, &url, &rep, &cancel)
                    .await
                    .map(|s| format!("Ollama {} en marcha", s.version.unwrap_or_default())),
                RuntimeKind::LlamaCpp => astrodev_installer::llamacpp::install(&hw, &rep, &cancel)
                    .await
                    .map(|b| format!("llama.cpp instalado en {}", b.display())),
            };
            rep.send(match res {
                Ok(s) => InstallEvent::Done(s),
                Err(e) => InstallEvent::Error(e.to_string()),
            });
            drop(rep);
            let _ = fwd.await;
        });
    }

    /// Lanza la instalación completa de un modelo del catálogo.
    pub fn start_install(&mut self, req: InstallRequest) {
        let cancel = CancellationToken::new();
        self.modal = Some(Modal::Install(InstallView::new(
            format!("Instalar {} con {}", req.model_id, req.runtime.label()),
            cancel.clone(),
        )));
        let tx = self.tx.clone();
        let url = self.ollama_url();
        let maker = self.provider_maker();
        tokio::spawn(async move {
            let (etx, mut erx) = mpsc::unbounded_channel();
            let fwd_tx = tx.clone();
            let fwd = tokio::spawn(async move {
                while let Some(e) = erx.recv().await {
                    let _ = fwd_tx.send(AppMsg::Install(e));
                }
            });
            let rep = Reporter::new(etx);
            rep.step("Detectando hardware");
            let hw =
                tokio::task::spawn_blocking(|| astrodev_hardware::detect(&astrodev_installer::paths::models_dir()))
                    .await
                    .expect("detección");
            let catalog = astrodev_hardware::Catalog::load(&astrodev_installer::paths::data_dir());
            let res =
                astrodev_installer::pipeline::install_model(&req, &hw, &catalog, &url, &maker, &rep, &cancel).await;
            match &res {
                Ok(o) => rep.send(InstallEvent::Done(o.summary.clone())),
                Err(e) => rep.send(InstallEvent::Error(e.to_string())),
            }
            drop(rep);
            let _ = fwd.await;
            let _ = tx.send(AppMsg::InstallDone(Box::new(res.map_err(|e| e.to_string()))));
        });
    }

    fn on_install_done(&mut self, r: Result<InstallOutcome, String>) {
        self.last_install = Some(r.as_ref().map(|o| o.summary.clone()).map_err(|e| e.clone()));
        match r {
            Ok(o) => {
                self.config.providers.entry(o.provider_name.clone()).or_insert(o.provider_config.clone());
                self.config.models.insert(o.model.clone(), o.settings.clone());
                self.config.default_provider = o.provider_name.clone();
                self.config.default_model = Some(o.model.clone());
                if let Err(e) = self.config.save(&self.config_path) {
                    self.entries.push(Entry::Error(format!("No se pudo guardar la configuración: {e}")));
                }
                self.switch_model(&format!("{}/{}", o.provider_name, o.model));
                self.entries.push(Entry::Info(format!("✓ {}. Ya es tu modelo por defecto.", o.summary)));
                self.refresh_stats();
            }
            Err(e) => self.entries.push(Entry::Error(format!("La instalación falló: {e}"))),
        }
    }

    fn uninstall(&mut self, name: String) {
        if let Some(rt) = RuntimeKind::parse(&name) {
            let r = match rt {
                RuntimeKind::Ollama => astrodev_installer::ollama::uninstall_managed(),
                RuntimeKind::LlamaCpp => astrodev_installer::llamacpp::uninstall_managed(),
            };
            match r {
                Ok(s) => self.entries.push(Entry::Info(s)),
                Err(e) => self.entries.push(Entry::Error(e.to_string())),
            }
            return;
        }
        let tx = self.tx.clone();
        let url = self.ollama_url();
        self.entries.push(Entry::Info(format!("Eliminando {name}…")));
        tokio::spawn(async move {
            let _ = tx.send(match astrodev_installer::pipeline::uninstall_model(&name, &url).await {
                Ok(s) => AppMsg::Info(s),
                Err(e) => AppMsg::Error(e.to_string()),
            });
        });
    }

    /// Cambia de modelo. Acepta `modelo` o `proveedor/modelo`.
    pub fn switch_model(&mut self, spec: &str) {
        let (prov, model) = match spec.split_once('/') {
            Some((p, m)) if self.config.providers.contains_key(p) => (Some(p.to_string()), m.to_string()),
            _ => (None, spec.to_string()),
        };
        let Ok(mut a) = self.agent.try_lock() else {
            return;
        };
        if let Some(p) = prov {
            match (self.factory)(&self.config, &p) {
                Ok(provider) => {
                    a.set_main(ModelRef::new(p.clone(), provider, model.clone()));
                    self.provider_name = p;
                }
                Err(e) => {
                    self.entries.push(Entry::Error(e));
                    return;
                }
            }
        } else {
            a.set_model(model.clone());
        }
        if let Some(cw) = self.config.models.get(&model).and_then(|m| m.context_window) {
            a.set_context_window(cw);
        }
        self.model = model.clone();
        self.entries.push(Entry::Info(format!("Modelo: {}/{model}", self.provider_name)));
    }

    pub fn context_pct(&self) -> u32 {
        if self.context_window == 0 {
            return 0;
        }
        ((self.context_tokens as f64 / self.context_window as f64) * 100.0).round() as u32
    }

    fn last_assistant(&mut self) -> &mut Entry {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            self.entries.push(Entry::Assistant { text: String::new(), thinking: String::new() });
        }
        self.entries.last_mut().unwrap()
    }

    pub fn on_msg(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Agent(ev) => self.on_agent_event(ev),
            AppMsg::TurnFinished => {
                self.busy = false;
                self.cancel = None;
            }
            AppMsg::Info(s) => self.entries.push(Entry::Info(s)),
            AppMsg::Error(s) => self.entries.push(Entry::Error(s)),
            AppMsg::Models { models, auto_select } => self.on_models(models, auto_select),
            AppMsg::Install(e) => {
                if let Some(Modal::Install(v)) = &mut self.modal {
                    v.on_event(e);
                }
            }
            AppMsg::InstallDone(r) => self.on_install_done(*r),
            AppMsg::OnboardingData(b) => {
                let (hw, cat, st, models) = *b;
                if let Some(Modal::Onboarding(o)) = &mut self.modal {
                    o.set_hardware(hw, &cat, st, models);
                }
            }
            AppMsg::ProviderTested(r) => {
                if let Some(Modal::Provider(w)) = &mut self.modal {
                    w.set_models(r);
                }
            }
            AppMsg::Installed(list) => {
                if let Some(Modal::Hardware(v)) = &mut self.modal {
                    v.set_installed(list);
                }
            }
            AppMsg::RefreshInstalled => self.fetch_installed(),
            AppMsg::Hardware(b) => {
                let (hw, cat) = *b;
                if let Some(Modal::Hardware(v)) = &mut self.modal {
                    v.set(hw, cat);
                }
            }
            AppMsg::Stats { context_tokens, context_window, cost, usage } => {
                self.context_tokens = context_tokens;
                self.context_window = context_window;
                self.cost = cost;
                self.usage = usage;
            }
        }
    }

    fn on_models(&mut self, models: Vec<String>, auto_select: bool) {
        if models.is_empty() {
            self.entries.push(Entry::Error(format!(
                "{} no tiene modelos instalados. Usa /hardware para ver cuál te cabe e /install para descargarlo.",
                self.provider_name
            )));
            return;
        }
        if auto_select && self.model.is_empty() {
            let pick = models.iter().find(|m| m.contains("coder") || m.contains("code")).unwrap_or(&models[0]).clone();
            self.switch_model(&pick);
            return;
        }
        if auto_select {
            let base = |s: &str| s.split(':').next().unwrap_or(s).to_string();
            let known = models.iter().any(|m| m == &self.model || (base(m) == self.model && m.ends_with(":latest")));
            if !known && self.provider_name.contains("ollama") {
                self.entries.push(Entry::Error(format!(
                    "El modelo {} no está instalado. Disponibles: {}",
                    self.model,
                    models.join(", ")
                )));
            }
            return;
        }
        let list = models
            .iter()
            .map(|m| if *m == self.model { format!("  ● {m}") } else { format!("    {m}") })
            .collect::<Vec<_>>()
            .join("\n");
        self.entries.push(Entry::Info(format!(
            "Modelos en {}:\n{list}\nCambia con /model <nombre> o /model <proveedor>/<modelo>",
            self.provider_name
        )));
    }

    fn on_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta(t) => {
                if let Entry::Assistant { text, .. } = self.last_assistant() {
                    text.push_str(&t);
                }
            }
            AgentEvent::ThinkingDelta(t) => {
                if let Entry::Assistant { thinking, .. } = self.last_assistant() {
                    thinking.push_str(&t);
                }
            }
            AgentEvent::ToolStart(call) => self.entries.push(Entry::Tool { call, status: ToolStatus::Running }),
            AgentEvent::ToolEnd { call, result } => {
                let status = match result {
                    Ok(o) => ToolStatus::Ok { summary: o.content, diff: o.diff.filter(|d| !d.is_empty()) },
                    Err(e) => ToolStatus::Failed(e),
                };
                if let Some(Entry::Tool { status: s, .. }) =
                    self.entries.iter_mut().rev().find(|e| matches!(e, Entry::Tool { call: c, .. } if c.id == call.id))
                {
                    *s = status;
                }
            }
            AgentEvent::PermissionRequest(req) => {
                self.scroll_from_bottom = 0;
                self.modal = Some(Modal::Permission { req, scroll: 0, feedback: None });
            }
            AgentEvent::Usage(u) => self.usage.add(u),
            AgentEvent::Stats { tokens_per_second } => self.last_tps = Some(tokens_per_second),
            AgentEvent::Compacted { before_tokens, after_tokens } => {
                self.context_tokens = after_tokens;
                self.entries
                    .push(Entry::Info(format!("Contexto compactado: ~{before_tokens} → ~{after_tokens} tokens.")));
            }
            AgentEvent::Notice(n) => self.entries.push(Entry::Info(n)),
            AgentEvent::TurnDone => {}
            AgentEvent::Cancelled => {
                if let Some(Modal::Permission { .. }) = self.modal {
                    self.modal = None;
                }
                self.entries.push(Entry::Info("Cancelado.".into()))
            }
            AgentEvent::Error(e) => self.entries.push(Entry::Error(e)),
        }
    }
}

pub fn mode_name(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Ask => "ask",
        PermissionMode::AutoProject => "auto-project",
        PermissionMode::AutoAll => "auto-all",
    }
}

/// Reconstruye la vista del chat a partir del historial guardado.
pub fn entries_from_messages(msgs: &[Message]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for m in msgs {
        match m.role {
            Role::User => out.push(Entry::User(m.content.clone())),
            Role::Assistant => {
                if !m.content.trim().is_empty() {
                    out.push(Entry::Assistant { text: m.content.clone(), thinking: String::new() });
                }
                for c in &m.tool_calls {
                    out.push(Entry::Tool { call: c.clone(), status: ToolStatus::Running });
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                let status = if let Some(e) = m.content.strip_prefix("ERROR: ") {
                    ToolStatus::Failed(e.to_string())
                } else {
                    ToolStatus::Ok { summary: m.content.clone(), diff: None }
                };
                if let Some(Entry::Tool { status: s, .. }) =
                    out.iter_mut().rev().find(|e| matches!(e, Entry::Tool { call, .. } if call.id == id))
                {
                    *s = status;
                }
            }
            Role::System => {}
        }
    }
    // Llamadas sin resultado (sesión interrumpida).
    for e in out.iter_mut() {
        if let Entry::Tool { status, .. } = e {
            if *status == ToolStatus::Running {
                *status = ToolStatus::Failed("sin resultado (sesión interrumpida)".into());
            }
        }
    }
    out
}
