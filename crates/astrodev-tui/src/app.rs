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
use crate::input::Input;

/// Construye un proveedor a partir de su nombre en la configuración.
pub type ProviderFactory =
    Arc<dyn Fn(&Config, &str) -> Result<Arc<dyn Provider>, String> + Send + Sync>;

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
    Ok {
        summary: String,
        diff: Option<String>,
    },
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
            agent: agent.clone(),
        };
        if !a.messages().is_empty() {
            app.entries.extend(entries_from_messages(a.messages()));
            app.entries.push(Entry::Info(format!(
                "Sesión reanudada: {}",
                a.session().title
            )));
        }
        drop(a);
        app
    }

    /// Comprueba el proveedor al arrancar y, si hace falta, elige modelo.
    pub fn startup_checks(&mut self, auto_select: bool) {
        self.fetch_models(auto_select);
    }

    fn fetch_models(&self, auto_select: bool) {
        let agent = self.agent.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let provider = agent.lock().await.provider();
            match provider.list_models().await {
                Ok(models) => {
                    let _ = tx.send(AppMsg::Models {
                        models: models.into_iter().map(|m| m.id).collect(),
                        auto_select,
                    });
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
        if self.busy {
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
                Some(Modal::Permission {
                    feedback: Some(i), ..
                }) => i.insert_str(&s),
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
            Modal::Permission {
                req,
                scroll,
                feedback,
            } => {
                if let Some(input) = feedback {
                    match k.code {
                        KeyCode::Enter => {
                            let text = input.text().trim().to_string();
                            req.respond(PermissionReply::Deny {
                                feedback: (!text.is_empty()).then_some(text),
                            });
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
                    KeyCode::Char('s')
                    | KeyCode::Char('y')
                    | KeyCode::Char('1')
                    | KeyCode::Enter => Some(PermissionReply::AllowOnce),
                    KeyCode::Char('a') | KeyCode::Char('2') if !req.dangerous => {
                        Some(PermissionReply::AllowSession)
                    }
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
            PermissionMode::AutoAll => {
                "auto-all — todo automático (los comandos peligrosos siguen preguntando)"
            }
        };
        self.entries
            .push(Entry::Info(format!("Modo de permisos: {desc}")));
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
        if let Some(cmd) = commands::parse(&text) {
            self.run_command(cmd);
            return;
        }
        self.send_prompt(text.clone(), text);
    }

    /// Envía un turno al agente. `shown` es lo que se ve en el chat.
    pub fn send_prompt(&mut self, shown: String, prompt: String) {
        if self.busy {
            self.entries.push(Entry::Error(
                "Espera a que termine la respuesta actual (Esc para cancelar).".into(),
            ));
            return;
        }
        if self.model.is_empty() {
            self.entries.push(Entry::Error(
                "No hay modelo seleccionado. Usa /model, /install o /provider add.".into(),
            ));
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
            self.entries.push(Entry::Error(
                "Espera a que termine la respuesta actual.".into(),
            ));
            return false;
        }
        true
    }

    fn resume(&mut self, id: &str) {
        let Ok(mut a) = self.agent.try_lock() else {
            self.entries
                .push(Entry::Error("El agente está ocupado.".into()));
            return;
        };
        let Some(store) = a.store() else {
            self.entries.push(Entry::Error(
                "No hay almacén de sesiones configurado.".into(),
            ));
            return;
        };
        match store.load(id) {
            Ok(s) => {
                let title = s.title.clone();
                a.load_session(s);
                self.model = a.model().to_string();
                self.entries = vec![Entry::Welcome];
                self.entries.extend(entries_from_messages(a.messages()));
                self.entries
                    .push(Entry::Info(format!("Sesión reanudada: {title}")));
                self.usage = a.usage();
                self.cost = a.cost_usd();
                self.context_tokens = a.context_tokens();
            }
            Err(e) => self
                .entries
                .push(Entry::Error(format!("No se pudo cargar la sesión: {e}"))),
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
                let prompt = "Analiza este proyecto (estructura, lenguajes, cómo se compila, cómo se ejecutan los tests, \
                    convenciones de código visibles) y crea un archivo ASTRO.md en la raíz con esa información, conciso \
                    (máx. 60 líneas) y útil para un agente de programación. Si ya existe, mejóralo sin perder lo que haya."
                    .to_string();
                self.send_prompt("/init — generar ASTRO.md".into(), prompt);
            }
            Command::Hardware | Command::Install(_) | Command::Uninstall(_) | Command::Provider(_) => {
                self.entries.push(Entry::Info("Este comando aún no está disponible en esta compilación.".into()))
            }
            Command::Unknown(n) => self.entries.push(Entry::Error(format!("Comando desconocido /{n}. Escribe /help."))),
        }
    }

    /// Cambia de modelo. Acepta `modelo` o `proveedor/modelo`.
    pub fn switch_model(&mut self, spec: &str) {
        let (prov, model) = match spec.split_once('/') {
            Some((p, m)) if self.config.providers.contains_key(p) => {
                (Some(p.to_string()), m.to_string())
            }
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
        if let Some(cw) = self
            .config
            .models
            .get(&model)
            .and_then(|m| m.context_window)
        {
            a.set_context_window(cw);
        }
        self.model = model.clone();
        self.entries.push(Entry::Info(format!(
            "Modelo: {}/{model}",
            self.provider_name
        )));
    }

    pub fn context_pct(&self) -> u32 {
        if self.context_window == 0 {
            return 0;
        }
        ((self.context_tokens as f64 / self.context_window as f64) * 100.0).round() as u32
    }

    fn last_assistant(&mut self) -> &mut Entry {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            self.entries.push(Entry::Assistant {
                text: String::new(),
                thinking: String::new(),
            });
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
            AppMsg::Models {
                models,
                auto_select,
            } => self.on_models(models, auto_select),
            AppMsg::Stats {
                context_tokens,
                context_window,
                cost,
                usage,
            } => {
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
            let pick = models
                .iter()
                .find(|m| m.contains("coder") || m.contains("code"))
                .unwrap_or(&models[0])
                .clone();
            self.switch_model(&pick);
            return;
        }
        if auto_select {
            let base = |s: &str| s.split(':').next().unwrap_or(s).to_string();
            let known = models
                .iter()
                .any(|m| m == &self.model || (base(m) == self.model && m.ends_with(":latest")));
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
            .map(|m| {
                if *m == self.model {
                    format!("  ● {m}")
                } else {
                    format!("    {m}")
                }
            })
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
            AgentEvent::ToolStart(call) => self.entries.push(Entry::Tool {
                call,
                status: ToolStatus::Running,
            }),
            AgentEvent::ToolEnd { call, result } => {
                let status = match result {
                    Ok(o) => ToolStatus::Ok {
                        summary: o.content,
                        diff: o.diff.filter(|d| !d.is_empty()),
                    },
                    Err(e) => ToolStatus::Failed(e),
                };
                if let Some(Entry::Tool { status: s, .. }) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { call: c, .. } if c.id == call.id))
                {
                    *s = status;
                }
            }
            AgentEvent::PermissionRequest(req) => {
                self.scroll_from_bottom = 0;
                self.modal = Some(Modal::Permission {
                    req,
                    scroll: 0,
                    feedback: None,
                });
            }
            AgentEvent::Usage(u) => self.usage.add(u),
            AgentEvent::Stats { tokens_per_second } => self.last_tps = Some(tokens_per_second),
            AgentEvent::Compacted {
                before_tokens,
                after_tokens,
            } => {
                self.context_tokens = after_tokens;
                self.entries.push(Entry::Info(format!(
                    "Contexto compactado: ~{before_tokens} → ~{after_tokens} tokens."
                )));
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
                    out.push(Entry::Assistant {
                        text: m.content.clone(),
                        thinking: String::new(),
                    });
                }
                for c in &m.tool_calls {
                    out.push(Entry::Tool {
                        call: c.clone(),
                        status: ToolStatus::Running,
                    });
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                let status = if let Some(e) = m.content.strip_prefix("ERROR: ") {
                    ToolStatus::Failed(e.to_string())
                } else {
                    ToolStatus::Ok {
                        summary: m.content.clone(),
                        diff: None,
                    }
                };
                if let Some(Entry::Tool { status: s, .. }) = out
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Tool { call, .. } if call.id == id))
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
