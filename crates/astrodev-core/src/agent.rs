//! Bucle agente: el modelo responde, pide herramientas, observa resultados e itera.
//!
//! Fase 1: política sólo-lectura (el modelo sólo ve herramientas `ReadOnly`).
//! Fase 2 añade permisos interactivos, prompt-tools, resumen automático y sesiones.

use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::message::{Message, ToolCall};
use crate::provider::{ChatRequest, Provider, ProviderError, StreamEvent, Usage};
use crate::tool::{ToolContext, ToolEffect, ToolOutput, ToolRegistry, ToolSpec};

/// Eventos que el agente emite hacia la interfaz.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart(ToolCall),
    ToolEnd {
        call: ToolCall,
        result: Result<ToolOutput, String>,
    },
    Usage(Usage),
    Stats {
        tokens_per_second: f64,
    },
    /// Fin del turno (el modelo dejó de pedir herramientas).
    TurnDone,
    Cancelled,
    Error(String),
}

/// Qué herramientas puede usar el modelo sin intervención del usuario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Sin herramientas (chat puro).
    None,
    /// Sólo herramientas de lectura.
    ReadOnly,
    /// Todas (la confirmación la gestiona la capa de permisos).
    All,
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    model: String,
    messages: Vec<Message>,
    system_prompt: String,
    ctx: ToolContext,
    config: AgentConfig,
    policy: ToolPolicy,
    usage: Usage,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
        model: impl Into<String>,
        ctx: ToolContext,
        config: AgentConfig,
    ) -> Self {
        let mut a = Self {
            provider,
            tools,
            model: model.into(),
            messages: Vec::new(),
            system_prompt: String::new(),
            ctx,
            config,
            policy: ToolPolicy::ReadOnly,
            usage: Usage::default(),
        };
        a.system_prompt = crate::system_prompt::build(&a.ctx.project, &a.visible_tools());
        a
    }

    pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = policy;
        self.system_prompt = crate::system_prompt::build(&self.ctx.project, &self.visible_tools());
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn provider(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }

    pub fn project_root(&self) -> &std::path::Path {
        self.ctx.project.root()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Herramientas que se anuncian al modelo según la política.
    pub fn visible_tools(&self) -> Vec<ToolSpec> {
        match self.policy {
            ToolPolicy::None => Vec::new(),
            ToolPolicy::All => self.tools.specs(),
            ToolPolicy::ReadOnly => self
                .tools
                .names()
                .into_iter()
                .filter_map(|n| self.tools.get(&n))
                .filter(|t| t.effect() == ToolEffect::ReadOnly)
                .map(|t| t.spec())
                .collect(),
        }
    }

    fn request(&self) -> ChatRequest {
        let mut messages = Vec::with_capacity(self.messages.len() + 1);
        messages.push(Message::system(self.system_prompt.clone()));
        messages.extend(self.messages.iter().cloned());
        ChatRequest {
            model: self.model.clone(),
            messages,
            tools: self.visible_tools(),
            temperature: self.config.temperature,
            max_tokens: None,
            context_window: Some(self.config.default_context_window),
        }
    }

    /// Ejecuta un turno completo a partir de la entrada del usuario.
    pub async fn run_turn(
        &mut self,
        input: impl Into<String>,
        tx: mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) {
        self.messages.push(Message::user(input));
        let mut ctx = self.ctx.clone();
        ctx.cancel = cancel.clone();

        for step in 0..self.config.max_steps {
            tracing::debug!(step, "agent step");
            let stream = match self
                .provider
                .chat_stream(self.request(), cancel.clone())
                .await
            {
                Ok(s) => s,
                Err(ProviderError::Cancelled) => {
                    let _ = tx.send(AgentEvent::Cancelled).await;
                    return;
                }
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                    return;
                }
            };
            let mut stream = stream;
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(StreamEvent::TextDelta(t)) => {
                        text.push_str(&t);
                        let _ = tx.send(AgentEvent::TextDelta(t)).await;
                    }
                    Ok(StreamEvent::ThinkingDelta(t)) => {
                        let _ = tx.send(AgentEvent::ThinkingDelta(t)).await;
                    }
                    Ok(StreamEvent::ToolCall(c)) => calls.push(c),
                    Ok(StreamEvent::Usage(u)) => {
                        self.usage.add(u);
                        let _ = tx.send(AgentEvent::Usage(u)).await;
                    }
                    Ok(StreamEvent::Stats { tokens_per_second }) => {
                        let _ = tx.send(AgentEvent::Stats { tokens_per_second }).await;
                    }
                    Ok(StreamEvent::Done) => break,
                    Err(ProviderError::Cancelled) => {
                        self.finish_partial(text, Vec::new());
                        let _ = tx.send(AgentEvent::Cancelled).await;
                        return;
                    }
                    Err(e) => {
                        self.finish_partial(text, Vec::new());
                        let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                        return;
                    }
                }
            }

            self.messages
                .push(Message::assistant_with_tools(text, calls.clone()));
            if calls.is_empty() {
                let _ = tx.send(AgentEvent::TurnDone).await;
                return;
            }

            for call in calls {
                if cancel.is_cancelled() {
                    self.messages
                        .push(Message::tool_result(&call, "cancelado por el usuario"));
                    continue;
                }
                let _ = tx.send(AgentEvent::ToolStart(call.clone())).await;
                let result = self.execute_tool(&call, &ctx).await;
                let content = match &result {
                    Ok(o) => o.content.clone(),
                    Err(e) => format!("ERROR: {e}"),
                };
                self.messages.push(Message::tool_result(&call, content));
                let _ = tx.send(AgentEvent::ToolEnd { call, result }).await;
            }
            if cancel.is_cancelled() {
                let _ = tx.send(AgentEvent::Cancelled).await;
                return;
            }
        }
        let _ = tx
            .send(AgentEvent::Error(format!(
                "se alcanzó el máximo de {} pasos en este turno",
                self.config.max_steps
            )))
            .await;
    }

    fn finish_partial(&mut self, text: String, calls: Vec<ToolCall>) {
        if !text.is_empty() || !calls.is_empty() {
            self.messages
                .push(Message::assistant_with_tools(text, calls));
        }
    }

    async fn execute_tool(&self, call: &ToolCall, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let Some(tool) = self.tools.get(&call.name) else {
            return Err(format!(
                "herramienta desconocida `{}`. Disponibles: {}",
                call.name,
                self.visible_tools()
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let allowed = match self.policy {
            ToolPolicy::None => false,
            ToolPolicy::ReadOnly => tool.effect() == ToolEffect::ReadOnly,
            ToolPolicy::All => true,
        };
        if !allowed {
            return Err(format!(
                "la herramienta `{}` no está permitida en el modo actual",
                call.name
            ));
        }
        tool.execute(&call.arguments, ctx)
            .await
            .map_err(|e| e.to_string())
    }
}
