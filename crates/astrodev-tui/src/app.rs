//! Estado de la aplicación y manejo de eventos.

use std::sync::Arc;

use astrodev_core::provider::Usage;
use astrodev_core::{Agent, AgentEvent, ToolCall};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::commands::{self, Command};
use crate::input::Input;

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
    pub has_astro_md: bool,
    pub usage: Usage,
    pub last_tps: Option<f64>,
    pub spinner: usize,
    pub should_quit: bool,
    pub tool_names: Vec<String>,
    tx: mpsc::UnboundedSender<AppMsg>,
}

impl App {
    pub async fn new(
        agent: Arc<Mutex<Agent>>,
        provider_name: String,
        tx: mpsc::UnboundedSender<AppMsg>,
    ) -> Self {
        let (model, project, has_astro_md, tool_names) = {
            let a = agent.lock().await;
            (
                a.model().to_string(),
                a.project_root().display().to_string(),
                a.project_root()
                    .join(astrodev_core::project::CONTEXT_FILE)
                    .exists(),
                a.visible_tools().into_iter().map(|t| t.name).collect(),
            )
        };
        Self {
            entries: vec![Entry::Welcome],
            input: Input::default(),
            scroll_from_bottom: 0,
            busy: false,
            cancel: None,
            agent,
            provider_name,
            model,
            project,
            has_astro_md,
            usage: Usage::default(),
            last_tps: None,
            spinner: 0,
            should_quit: false,
            tool_names,
            tx,
        }
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
                    let _ = tx.send(AppMsg::Error(e.to_string()));
                }
            }
        });
    }

    pub fn tick(&mut self) {
        if self.busy {
            self.spinner = self.spinner.wrapping_add(1);
        }
    }

    pub fn on_terminal_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(s) => self.input.insert_str(&s),
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => self.scroll_up(3),
                MouseEventKind::ScrollDown => self.scroll_down(3),
                _ => {}
            },
            _ => {}
        }
    }

    fn scroll_up(&mut self, n: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(n);
    }

    fn scroll_down(&mut self, n: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n);
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
        if self.busy {
            self.entries.push(Entry::Error(
                "Espera a que termine la respuesta actual (Esc para cancelar).".into(),
            ));
            return;
        }
        if self.model.is_empty() {
            self.entries.push(Entry::Error(
                "No hay modelo seleccionado. Usa /model para ver los disponibles.".into(),
            ));
            return;
        }
        self.entries.push(Entry::User(text.clone()));
        self.start_turn(text);
    }

    fn start_turn(&mut self, text: String) {
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
            agent.lock().await.run_turn(text, atx, cancel).await;
            let _ = fwd.await;
            let _ = tx.send(AppMsg::TurnFinished);
        });
    }

    fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::Help => self.entries.push(Entry::Info(commands::HELP.into())),
            Command::Quit => self.should_quit = true,
            Command::Clear => {
                if self.busy {
                    self.entries.push(Entry::Error("No se puede limpiar durante una respuesta.".into()));
                    return;
                }
                if let Ok(mut a) = self.agent.try_lock() {
                    a.clear();
                }
                self.entries = vec![Entry::Welcome];
                self.entries.push(Entry::Info("Conversación borrada.".into()));
            }
            Command::Model(None) => {
                self.entries.push(Entry::Info(format!("Consultando modelos de {}…", self.provider_name)));
                self.fetch_models(false);
            }
            Command::Model(Some(m)) => {
                if self.busy {
                    self.entries.push(Entry::Error("Espera a que termine la respuesta actual.".into()));
                    return;
                }
                if let Ok(mut a) = self.agent.try_lock() {
                    a.set_model(m.clone());
                }
                self.model = m.clone();
                self.entries.push(Entry::Info(format!("Modelo cambiado a {m}")));
            }
            Command::Tools => self.entries.push(Entry::Info(format!(
                "Herramientas que puede usar el modelo (fase 1: sólo lectura):\n  {}\n\
                 Escritura y shell se activan con el sistema de permisos (fase 2).\n\
                 Mientras tanto puedes probarlas con `astrodev tool <nombre> '<json>'`.",
                self.tool_names.join(", ")
            ))),
            Command::Cost => self.entries.push(Entry::Info(format!(
                "Tokens en esta sesión: {} entrada · {} salida{}\n(Los modelos locales no tienen coste; el cálculo de coste de APIs llega en la fase 5.)",
                self.usage.input_tokens,
                self.usage.output_tokens,
                self.last_tps.map(|t| format!(" · última velocidad {t:.1} tok/s")).unwrap_or_default()
            ))),
            Command::Planned { name, phase } => self.entries.push(Entry::Info(format!(
                "/{name} estará disponible en la fase {phase}."
            ))),
            Command::Unknown(n) => self.entries.push(Entry::Error(format!(
                "Comando desconocido /{n}. Escribe /help."
            ))),
        }
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
        }
    }

    fn on_models(&mut self, models: Vec<String>, auto_select: bool) {
        if models.is_empty() {
            self.entries.push(Entry::Error(format!(
                "{} no tiene modelos instalados. Prueba: ollama pull qwen2.5-coder:7b",
                self.provider_name
            )));
            return;
        }
        if auto_select && self.model.is_empty() {
            // Preferimos modelos de código si los hay.
            let pick = models
                .iter()
                .find(|m| m.contains("coder") || m.contains("code"))
                .unwrap_or(&models[0])
                .clone();
            if let Ok(mut a) = self.agent.try_lock() {
                a.set_model(pick.clone());
            }
            self.model = pick.clone();
            self.entries.push(Entry::Info(format!(
                "Modelo seleccionado automáticamente: {pick} (cámbialo con /model <nombre>)"
            )));
            return;
        }
        if auto_select {
            if !models
                .iter()
                .any(|m| m == &self.model || m.split(':').next() == Some(&self.model))
            {
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
            "Modelos en {}:\n{list}\nCambia con /model <nombre>",
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
            AgentEvent::Usage(u) => self.usage.add(u),
            AgentEvent::Stats { tokens_per_second } => self.last_tps = Some(tokens_per_second),
            AgentEvent::TurnDone => {}
            AgentEvent::Cancelled => self.entries.push(Entry::Info("Cancelado.".into())),
            AgentEvent::Error(e) => self.entries.push(Entry::Error(e)),
        }
    }
}
