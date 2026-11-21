//! Bucle agente: el modelo responde, pide herramientas, se comprueban permisos (con
//! vista previa del diff), se ejecutan, se observa el resultado y se itera.
//!
//! Incluye: tool calling nativo o por prompt (con reintentos ante llamadas mal
//! formadas), checkpoints para `/undo`, resumen automático al acercarse al límite de
//! contexto, perfiles de modelo (planificar / editar / resumir), fallback entre
//! modelos y persistencia de sesiones.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::checkpoint::UndoReport;
use crate::config::{AgentConfig, ToolMode};
use crate::context;
use crate::message::{Message, ToolCall};
use crate::permissions::{Decision, PermissionReply, Permissions};
use crate::prompt_tools::{self, StreamFilter};
use crate::provider::{ChatRequest, EventStream, Provider, ProviderError, StreamEvent, Usage};
use crate::session::{Session, SessionStore};
use crate::tool::{ToolContext, ToolEffect, ToolError, ToolOutput, ToolRegistry};

/// Un modelo concreto de un proveedor concreto.
#[derive(Clone)]
pub struct ModelRef {
    pub provider_name: String,
    pub provider: Arc<dyn Provider>,
    pub model: String,
}

impl ModelRef {
    pub fn new(
        provider_name: impl Into<String>,
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider_name: provider_name.into(),
            provider,
            model: model.into(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.provider_name, self.model)
    }
}

/// Petición de permiso enviada a la interfaz. Se responde con [`PermissionRequest::respond`].
#[derive(Clone)]
pub struct PermissionRequest {
    pub call: ToolCall,
    pub reason: String,
    /// Peligroso o fuera del proyecto: no se ofrece "permitir siempre".
    pub dangerous: bool,
    /// Diff u otra previsualización.
    pub preview: Option<String>,
    reply: Arc<Mutex<Option<oneshot::Sender<PermissionReply>>>>,
}

impl PermissionRequest {
    pub fn respond(&self, r: PermissionReply) {
        if let Some(tx) = self.reply.lock().unwrap().take() {
            let _ = tx.send(r);
        }
    }
}

impl std::fmt::Debug for PermissionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionRequest")
            .field("call", &self.call)
            .field("reason", &self.reason)
            .field("dangerous", &self.dangerous)
            .finish()
    }
}

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
    PermissionRequest(PermissionRequest),
    Usage(Usage),
    Stats {
        tokens_per_second: f64,
    },
    /// Se resumió la conversación para liberar contexto.
    Compacted {
        before_tokens: usize,
        after_tokens: usize,
    },
    /// Se usó otro modelo (fallback) o se cambió de modo de herramientas.
    Notice(String),
    TurnDone,
    Cancelled,
    Error(String),
}

type CostFn = Arc<dyn Fn(&str, &str, Usage) -> Option<f64> + Send + Sync>;

enum CallOutcome {
    Continue,
    /// El usuario denegó sin explicación: se detiene el turno.
    Stop,
}

pub struct Agent {
    main: ModelRef,
    planner: Option<ModelRef>,
    summarizer: Option<ModelRef>,
    fallbacks: Vec<ModelRef>,
    tools: ToolRegistry,
    ctx: ToolContext,
    config: AgentConfig,
    permissions: Permissions,
    tool_mode: ToolMode,
    /// Modo efectivo para el modelo principal (resuelto desde `Auto`).
    resolved_mode: Option<ToolMode>,
    context_window: u32,
    session: Session,
    store: Option<SessionStore>,
    cost_fn: Option<CostFn>,
    call_counter: u64,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
        model: impl Into<String>,
        ctx: ToolContext,
        config: AgentConfig,
    ) -> Self {
        let name = provider.name().to_string();
        let model = model.into();
        let session = Session::new(ctx.project.root(), &name, &model);
        Self {
            main: ModelRef::new(name, provider, model),
            planner: None,
            summarizer: None,
            fallbacks: Vec::new(),
            tools,
            ctx,
            context_window: config.default_context_window,
            config,
            permissions: Permissions::new(&Default::default()),
            tool_mode: ToolMode::Auto,
            resolved_mode: None,
            session,
            store: None,
            cost_fn: None,
            call_counter: 0,
        }
    }

    // ----- configuración (builder) -----

    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.main.provider_name = name.into();
        self.session.provider = self.main.provider_name.clone();
        self
    }
    pub fn with_permissions(mut self, p: Permissions) -> Self {
        self.permissions = p;
        self
    }
    pub fn with_tool_mode(mut self, m: ToolMode) -> Self {
        self.tool_mode = m;
        self.resolved_mode = None;
        self
    }
    pub fn with_context_window(mut self, n: u32) -> Self {
        self.context_window = n.max(1024);
        self
    }
    pub fn with_store(mut self, s: SessionStore) -> Self {
        self.store = Some(s);
        self
    }
    pub fn with_planner(mut self, m: Option<ModelRef>) -> Self {
        self.planner = m;
        self
    }
    pub fn with_summarizer(mut self, m: Option<ModelRef>) -> Self {
        self.summarizer = m;
        self
    }
    pub fn with_fallbacks(mut self, f: Vec<ModelRef>) -> Self {
        self.fallbacks = f;
        self
    }
    pub fn with_cost_fn(mut self, f: CostFn) -> Self {
        self.cost_fn = Some(f);
        self
    }

    // ----- acceso -----

    pub fn model(&self) -> &str {
        &self.main.model
    }
    pub fn provider_name(&self) -> &str {
        &self.main.provider_name
    }
    pub fn provider(&self) -> Arc<dyn Provider> {
        self.main.provider.clone()
    }
    pub fn main_model(&self) -> &ModelRef {
        &self.main
    }
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.main.model = model.into();
        self.session.model = self.main.model.clone();
        self.resolved_mode = None;
    }
    pub fn set_main(&mut self, m: ModelRef) {
        self.main = m;
        self.session.model = self.main.model.clone();
        self.session.provider = self.main.provider_name.clone();
        self.resolved_mode = None;
    }
    pub fn set_context_window(&mut self, n: u32) {
        self.context_window = n.max(1024);
    }
    pub fn context_window(&self) -> u32 {
        self.context_window
    }
    pub fn project_root(&self) -> &std::path::Path {
        self.ctx.project.root()
    }
    pub fn messages(&self) -> &[Message] {
        &self.session.messages
    }
    pub fn usage(&self) -> Usage {
        self.session.usage
    }
    pub fn cost_usd(&self) -> f64 {
        self.session.cost_usd
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn permissions_mut(&mut self) -> &mut Permissions {
        &mut self.permissions
    }
    pub fn permission_mode(&self) -> crate::config::PermissionMode {
        self.permissions.mode
    }
    pub fn tool_mode(&self) -> ToolMode {
        self.resolved_mode.unwrap_or(self.tool_mode)
    }
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }
    pub fn store(&self) -> Option<&SessionStore> {
        self.store.as_ref()
    }

    /// Tokens estimados del contexto actual (sistema + historial).
    pub fn context_tokens(&self) -> usize {
        let sys = crate::system_prompt::build(&self.ctx.project, &self.tools.specs()).len() / 4;
        sys + context::estimate(&self.session.messages)
    }

    /// Empieza una sesión nueva (mantiene modelo y permisos de sesión).
    pub fn new_session(&mut self) {
        self.session = Session::new(
            self.ctx.project.root(),
            &self.main.provider_name,
            &self.main.model,
        );
    }

    /// Reanuda una sesión guardada.
    pub fn load_session(&mut self, s: Session) {
        if !s.model.is_empty() {
            self.main.model = s.model.clone();
        }
        self.resolved_mode = None;
        self.session = s;
    }

    pub fn save(&mut self) {
        if self.session.messages.is_empty() {
            return;
        }
        self.session.derive_title();
        self.session.updated_at = chrono::Utc::now();
        if let Some(store) = &self.store {
            if let Err(e) = store.save(&self.session) {
                tracing::warn!("no se pudo guardar la sesión: {e}");
            }
        }
    }

    /// Deshace el último turno: restaura archivos y recorta la conversación.
    pub fn undo(&mut self) -> Option<UndoReport> {
        let rep = self.session.checkpoints.undo()?;
        self.session.messages.truncate(rep.message_index);
        self.save();
        Some(rep)
    }

    fn next_call_id(&mut self) -> String {
        self.call_counter += 1;
        format!("call_{}_{}", self.session.id, self.call_counter)
    }

    // ----- bucle -----

    async fn resolve_mode(&mut self, tx: &mpsc::Sender<AgentEvent>) -> ToolMode {
        if let Some(m) = self.resolved_mode {
            return m;
        }
        let m = match self.tool_mode {
            ToolMode::Native => ToolMode::Native,
            ToolMode::Prompt => ToolMode::Prompt,
            ToolMode::Auto => match self
                .main
                .provider
                .probe_native_tools(&self.main.model)
                .await
            {
                Some(false) => {
                    let _ = tx
                        .send(AgentEvent::Notice(format!(
                            "{} no admite tool calling nativo: uso el modo por prompt.",
                            self.main.model
                        )))
                        .await;
                    ToolMode::Prompt
                }
                _ => ToolMode::Native,
            },
        };
        if let Some(cw) = self.main.provider.context_window(&self.main.model).await {
            // No superar la ventana real del modelo.
            if cw < self.context_window {
                self.context_window = cw;
            }
        }
        self.resolved_mode = Some(m);
        m
    }

    fn system_prompt(&self, mode: ToolMode) -> String {
        let specs = self.tools.specs();
        let mut s = crate::system_prompt::build(&self.ctx.project, &specs);
        if mode == ToolMode::Prompt {
            s.push_str(&prompt_tools::instructions(&specs));
        }
        s
    }

    fn build_request(&self, model: &ModelRef, mode: ToolMode) -> ChatRequest {
        let mut messages = Vec::with_capacity(self.session.messages.len() + 1);
        messages.push(Message::system(self.system_prompt(mode)));
        if mode == ToolMode::Prompt {
            messages.extend(prompt_tools::encode_history(&self.session.messages));
        } else {
            messages.extend(self.session.messages.iter().cloned());
        }
        ChatRequest {
            model: model.model.clone(),
            messages,
            tools: if mode == ToolMode::Prompt {
                Vec::new()
            } else {
                self.tools.specs()
            },
            temperature: self.config.temperature,
            max_tokens: None,
            context_window: Some(self.context_window),
        }
    }

    /// Abre el stream con el modelo elegido o, si falla, con los de fallback.
    async fn open_stream(
        &self,
        first: &ModelRef,
        mode: ToolMode,
        tx: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Result<(EventStream, ModelRef), ProviderError> {
        let mut candidates = vec![first.clone()];
        candidates.extend(
            self.fallbacks
                .iter()
                .filter(|f| f.label() != first.label())
                .cloned(),
        );
        let mut last_err = None;
        for (i, m) in candidates.iter().enumerate() {
            let mut req = self.build_request(m, mode);
            req.model = m.model.clone();
            match m.provider.chat_stream(req, cancel.clone()).await {
                Ok(s) => {
                    if i > 0 {
                        let _ = tx
                            .send(AgentEvent::Notice(format!(
                                "{} falló; usando fallback {}",
                                first.label(),
                                m.label()
                            )))
                            .await;
                    }
                    return Ok((s, m.clone()));
                }
                Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
                Err(e) if is_tools_unsupported(&e) => return Err(e),
                Err(e) => {
                    tracing::warn!(model = %m.label(), "error del proveedor: {e}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ProviderError::Other("sin modelos disponibles".into())))
    }

    /// Ejecuta un turno completo y guarda la sesión.
    pub async fn run_turn(
        &mut self,
        input: impl Into<String>,
        tx: mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) {
        let input = input.into();
        self.session
            .checkpoints
            .begin(self.session.messages.len(), input.clone());
        self.session.messages.push(Message::user(input));
        self.turn_loop(&tx, &cancel).await;
        self.save();
    }

    async fn turn_loop(&mut self, tx: &mpsc::Sender<AgentEvent>, cancel: &CancellationToken) {
        let mut mode = self.resolve_mode(tx).await;
        let mut retries = 0;
        let mut step = 0;
        while step < self.config.max_steps {
            if cancel.is_cancelled() {
                let _ = tx.send(AgentEvent::Cancelled).await;
                return;
            }
            self.maybe_compact(false, tx, cancel).await;

            let chosen = match (&self.planner, step) {
                (Some(p), 0) => p.clone(),
                _ => self.main.clone(),
            };
            let (mut stream, used) = match self.open_stream(&chosen, mode, tx, cancel).await {
                Ok(s) => s,
                Err(ProviderError::Cancelled) => {
                    let _ = tx.send(AgentEvent::Cancelled).await;
                    return;
                }
                Err(e) if mode == ToolMode::Native && is_tools_unsupported(&e) => {
                    mode = ToolMode::Prompt;
                    self.resolved_mode = Some(mode);
                    let _ = tx
                        .send(AgentEvent::Notice(format!(
                            "{} no admite tools nativas: cambio al modo por prompt.",
                            chosen.model
                        )))
                        .await;
                    continue;
                }
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                    return;
                }
            };
            step += 1;

            let mut raw = String::new();
            let mut native_calls: Vec<ToolCall> = Vec::new();
            let mut filter = StreamFilter::default();
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(StreamEvent::TextDelta(t)) => {
                        raw.push_str(&t);
                        let shown = filter.push(&t);
                        if !shown.is_empty() {
                            let _ = tx.send(AgentEvent::TextDelta(shown)).await;
                        }
                    }
                    Ok(StreamEvent::ThinkingDelta(t)) => {
                        let _ = tx.send(AgentEvent::ThinkingDelta(t)).await;
                    }
                    Ok(StreamEvent::ToolCall(c)) => native_calls.push(c),
                    Ok(StreamEvent::Usage(u)) => {
                        self.session.usage.add(u);
                        if let Some(f) = &self.cost_fn {
                            if let Some(c) = f(&used.provider_name, &used.model, u) {
                                self.session.cost_usd += c;
                            }
                        }
                        let _ = tx.send(AgentEvent::Usage(u)).await;
                    }
                    Ok(StreamEvent::Stats { tokens_per_second }) => {
                        let _ = tx.send(AgentEvent::Stats { tokens_per_second }).await;
                    }
                    Ok(StreamEvent::Done) => break,
                    Err(ProviderError::Cancelled) => {
                        if !raw.is_empty() {
                            self.session.messages.push(Message::assistant(raw));
                        }
                        let _ = tx.send(AgentEvent::Cancelled).await;
                        return;
                    }
                    Err(e) => {
                        if !raw.is_empty() {
                            self.session.messages.push(Message::assistant(raw));
                        }
                        let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                        return;
                    }
                }
            }
            let rest = filter.finish();
            if !rest.is_empty() {
                let _ = tx.send(AgentEvent::TextDelta(rest)).await;
            }

            // Llamadas en texto: siempre en modo prompt; en nativo sólo si el modelo
            // escribió explícitamente bloques <tool_call> (pasa con algunas plantillas).
            let (text, mut calls) = if native_calls.is_empty()
                && (mode == ToolMode::Prompt || raw.contains(prompt_tools::OPEN))
            {
                let parsed = prompt_tools::parse(&raw, &self.tools.names());
                if parsed.calls.is_empty() && !parsed.errors.is_empty() {
                    if retries < self.config.tool_parse_retries {
                        retries += 1;
                        self.session.messages.push(Message::assistant(raw));
                        self.session.messages.push(Message::user(format!(
                            "Tu llamada a herramienta no es válida: {}. Repite la llamada usando \
                             exactamente el formato:\n<tool_call>\n{{\"name\": \"NOMBRE\", \"arguments\": {{...}}}}\n</tool_call>",
                            parsed.errors.join("; ")
                        )));
                        let _ = tx
                            .send(AgentEvent::Notice(
                                "Llamada mal formada; pidiendo al modelo que la repita…".into(),
                            ))
                            .await;
                        continue;
                    }
                    let _ = tx
                        .send(AgentEvent::Error(format!(
                            "el modelo no generó una llamada válida tras {retries} reintentos: {}",
                            parsed.errors.join("; ")
                        )))
                        .await;
                }
                (parsed.text, parsed.calls)
            } else {
                (raw, native_calls)
            };
            for c in calls.iter_mut() {
                if c.id.is_empty() || c.id.starts_with("pt_") {
                    c.id = self.next_call_id();
                }
            }

            self.session
                .messages
                .push(Message::assistant_with_tools(text, calls.clone()));
            if calls.is_empty() {
                let _ = tx.send(AgentEvent::TurnDone).await;
                return;
            }

            for call in calls {
                if cancel.is_cancelled() {
                    self.session
                        .messages
                        .push(Message::tool_result(&call, "cancelado por el usuario"));
                    continue;
                }
                match self.handle_call(call, tx, cancel).await {
                    CallOutcome::Continue => {}
                    CallOutcome::Stop => {
                        let _ = tx.send(AgentEvent::TurnDone).await;
                        return;
                    }
                }
            }
        }
        let _ = tx
            .send(AgentEvent::Error(format!(
                "se alcanzó el máximo de {} pasos en este turno (configurable en [agent] max_steps)",
                self.config.max_steps
            )))
            .await;
    }

    async fn ask(
        &self,
        call: &ToolCall,
        reason: String,
        dangerous: bool,
        preview: Option<String>,
        tx: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> PermissionReply {
        let (otx, orx) = oneshot::channel();
        let req = PermissionRequest {
            call: call.clone(),
            reason,
            dangerous,
            preview,
            reply: Arc::new(Mutex::new(Some(otx))),
        };
        if tx.send(AgentEvent::PermissionRequest(req)).await.is_err() {
            return PermissionReply::Deny { feedback: None };
        }
        tokio::select! {
            r = orx => r.unwrap_or(PermissionReply::Deny { feedback: None }),
            _ = cancel.cancelled() => PermissionReply::Deny { feedback: None },
        }
    }

    /// Registra el resultado en el historial y lo notifica a la interfaz.
    async fn finish_call(
        &mut self,
        call: ToolCall,
        result: Result<ToolOutput, String>,
        tx: &mpsc::Sender<AgentEvent>,
    ) {
        let content = match &result {
            Ok(o) => o.content.clone(),
            Err(e) => format!("ERROR: {e}"),
        };
        self.session
            .messages
            .push(Message::tool_result(&call, content));
        let _ = tx.send(AgentEvent::ToolEnd { call, result }).await;
    }

    async fn handle_call(
        &mut self,
        call: ToolCall,
        tx: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> CallOutcome {
        let _ = tx.send(AgentEvent::ToolStart(call.clone())).await;
        let Some(tool) = self.tools.get(&call.name) else {
            let err = format!(
                "herramienta desconocida `{}`. Disponibles: {}",
                call.name,
                self.tools.names().join(", ")
            );
            self.finish_call(call, Err(err), tx).await;
            return CallOutcome::Continue;
        };
        let mut ctx = self.ctx.clone();
        ctx.cancel = cancel.clone();
        let effect = tool.effect();

        let mut decision = self
            .permissions
            .evaluate(&call.name, effect, &call.arguments);
        let mut outside = false;
        let mut preview = None;
        if effect == ToolEffect::WritesFiles {
            match tool.preview(&call.arguments, &ctx).await {
                Ok(p) => preview = p,
                Err(ToolError::OutsideProject(p)) => {
                    outside = true;
                    decision = Decision::Ask {
                        reason: format!("escribirá FUERA del proyecto: {p}"),
                        dangerous: true,
                    };
                }
                Err(e) => {
                    // La edición fallaría igual: no molestamos al usuario.
                    self.finish_call(call, Err(e.to_string()), tx).await;
                    return CallOutcome::Continue;
                }
            }
        }

        loop {
            match &decision {
                Decision::Allow => {}
                Decision::Deny(why) => {
                    self.finish_call(call, Err(why.clone()), tx).await;
                    return CallOutcome::Continue;
                }
                Decision::Ask { reason, dangerous } => {
                    let dangerous = *dangerous;
                    if outside {
                        ctx.allow_outside_project = true;
                        if preview.is_none() && effect == ToolEffect::WritesFiles {
                            preview = tool.preview(&call.arguments, &ctx).await.ok().flatten();
                        }
                    }
                    match self
                        .ask(
                            &call,
                            reason.clone(),
                            dangerous,
                            preview.clone(),
                            tx,
                            cancel,
                        )
                        .await
                    {
                        PermissionReply::AllowOnce => {}
                        PermissionReply::AllowSession => {
                            if !dangerous {
                                self.permissions.allow_for_session(&call.name);
                            }
                        }
                        PermissionReply::Deny { feedback } => {
                            let msg = match &feedback {
                                Some(f) => format!("El usuario denegó esta acción. Indicaciones del usuario: {f}"),
                                None => "El usuario denegó esta acción. Detente y espera nuevas instrucciones.".into(),
                            };
                            self.finish_call(call, Err(msg), tx).await;
                            return if feedback.is_some() {
                                CallOutcome::Continue
                            } else {
                                CallOutcome::Stop
                            };
                        }
                    }
                }
            }

            if effect == ToolEffect::WritesFiles {
                for p in tool.affected_paths(&call.arguments, &ctx) {
                    self.session.checkpoints.record(&p);
                }
            }
            match tool.execute(&call.arguments, &ctx).await {
                Err(ToolError::OutsideProject(p)) if !ctx.allow_outside_project => {
                    // Lectura fuera del proyecto: pedir permiso explícito y reintentar.
                    outside = true;
                    decision = Decision::Ask {
                        reason: format!("accederá FUERA del proyecto: {p}"),
                        dangerous: true,
                    };
                    continue;
                }
                Err(ToolError::Cancelled) => {
                    self.finish_call(call, Err("cancelado".into()), tx).await;
                    return CallOutcome::Continue;
                }
                res => {
                    self.finish_call(call, res.map_err(|e| e.to_string()), tx)
                        .await;
                    return CallOutcome::Continue;
                }
            }
        }
    }

    /// Compacta el contexto si se supera el umbral (o siempre, si `force`).
    pub async fn maybe_compact(
        &mut self,
        force: bool,
        tx: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) {
        let window = self.context_window as usize;
        let threshold = (window as f32 * self.config.summarize_at) as usize;
        let before = self.context_tokens();
        if !force && before < threshold {
            return;
        }
        // 1) Barato: recortar resultados antiguos de herramientas.
        context::shrink_old_tool_results(&mut self.session.messages, 6, 2000);
        if !force && self.context_tokens() < threshold {
            let _ = tx
                .send(AgentEvent::Compacted {
                    before_tokens: before,
                    after_tokens: self.context_tokens(),
                })
                .await;
            return;
        }
        // 2) Resumen con el modelo de resúmenes (o el principal).
        let Some(cut) = context::split_point(&self.session.messages, window * 35 / 100) else {
            return;
        };
        let m = self.summarizer.clone().unwrap_or_else(|| self.main.clone());
        let req = ChatRequest {
            model: m.model.clone(),
            messages: context::summary_request(&self.session.messages[..cut]),
            tools: Vec::new(),
            temperature: Some(0.2),
            max_tokens: Some(800),
            context_window: Some(self.context_window),
        };
        let summary = match m.provider.chat_stream(req, cancel.clone()).await {
            Ok(s) => crate::provider::collect(s).await.map(|r| r.text),
            Err(e) => Err(e),
        };
        match summary {
            Ok(s) if !s.trim().is_empty() => {
                context::apply_summary(&mut self.session.messages, cut, &s);
                // Los checkpoints anteriores al resumen ya no casan con los índices.
                self.session.checkpoints = Default::default();
                let _ = tx
                    .send(AgentEvent::Compacted {
                        before_tokens: before,
                        after_tokens: self.context_tokens(),
                    })
                    .await;
            }
            Ok(_) => {}
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Notice(format!(
                        "no se pudo resumir el contexto: {e}"
                    )))
                    .await;
            }
        }
    }
}

fn is_tools_unsupported(e: &ProviderError) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("does not support tools")
        || s.contains("tool use is not supported")
        || s.contains("tools is not supported")
}
