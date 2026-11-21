//! Integración: bucle agente + herramientas reales + proveedor falso (sin red).

use std::sync::Arc;

use astrodev_core::agent::ModelRef;
use astrodev_core::config::{AgentConfig, PermissionMode, PermissionsConfig, ToolMode};
use astrodev_core::permissions::{PermissionReply, Permissions};
use astrodev_core::provider::StreamEvent;
use astrodev_core::session::SessionStore;
use astrodev_core::{Agent, AgentEvent, Project, Role, ToolContext};
use astrodev_providers::MockProvider;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn setup() -> (tempfile::TempDir, Arc<MockProvider>, ToolContext) {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("hola.txt"), "contenido secreto\n").unwrap();
    std::fs::write(d.path().join("ASTRO.md"), "Usa siempre tabs.").unwrap();
    let ctx = ToolContext::new(Arc::new(Project::new(d.path()).unwrap()));
    (d, Arc::new(MockProvider::new()), ctx)
}

fn agent(mock: &Arc<MockProvider>, ctx: ToolContext, mode: PermissionMode) -> Agent {
    Agent::new(mock.clone(), astrodev_tools::default_registry(), "m", ctx, AgentConfig::default())
        .with_permissions(Permissions::new(&PermissionsConfig { mode, ..Default::default() }))
}

/// Ejecuta un turno respondiendo a los permisos con `reply` y devuelve los eventos.
async fn run(agent: &mut Agent, input: &str, reply: PermissionReply) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(256);
    let collector = tokio::spawn(async move {
        let mut evs = Vec::new();
        while let Some(e) = rx.recv().await {
            if let AgentEvent::PermissionRequest(r) = &e {
                r.respond(reply.clone());
            }
            evs.push(e);
        }
        evs
    });
    agent.run_turn(input, tx, CancellationToken::new()).await;
    collector.await.unwrap()
}

fn asked(evs: &[AgentEvent]) -> usize {
    evs.iter().filter(|e| matches!(e, AgentEvent::PermissionRequest(_))).count()
}

#[tokio::test]
async fn read_tools_run_without_asking() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("read_file", json!({"path": "hola.txt"}));
    mock.push_text("El archivo dice: contenido secreto");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "¿qué hay en hola.txt?", PermissionReply::Deny { feedback: None }).await;

    assert_eq!(asked(&evs), 0);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(o), .. } if o.content.contains("contenido secreto"))));
    assert!(matches!(evs.last(), Some(AgentEvent::TurnDone)));
    let roles: Vec<Role> = a.messages().iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]);
    let reqs = mock.requests();
    assert!(reqs[0].messages[0].content.contains("Usa siempre tabs."));
    assert!(reqs[0].tools.iter().any(|t| t.name == "edit_file"), "en modo nativo se anuncian todas");
}

#[tokio::test]
async fn edit_asks_with_diff_preview_and_applies() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("edit_file", json!({"path": "hola.txt", "old_string": "secreto", "new_string": "público"}));
    mock.push_text("hecho");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "cambia", PermissionReply::AllowOnce).await;
    let req = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequest(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert!(req.preview.unwrap().contains("+contenido público"));
    assert!(!req.dangerous);
    assert_eq!(std::fs::read_to_string(d.path().join("hola.txt")).unwrap(), "contenido público\n");
}

#[tokio::test]
async fn deny_stops_turn_and_keeps_file() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("write_file", json!({"path": "x.txt", "content": "pwned"}));
    mock.push_text("no debería llegar");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "escribe", PermissionReply::Deny { feedback: None }).await;
    assert!(!d.path().join("x.txt").exists());
    assert!(matches!(evs.last(), Some(AgentEvent::TurnDone)));
    assert_eq!(mock.requests().len(), 1, "tras denegar no se vuelve a llamar al modelo");
}

#[tokio::test]
async fn deny_with_feedback_continues() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("write_file", json!({"path": "x.txt", "content": "a"}));
    mock.push_text("vale, lo hago distinto");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    run(&mut a, "escribe", PermissionReply::Deny { feedback: Some("usa y.txt".into()) }).await;
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[1].messages.iter().any(|m| m.content.contains("usa y.txt")));
}

#[tokio::test]
async fn auto_project_writes_without_asking_but_commands_ask() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("write_file", json!({"path": "x.txt", "content": "ok"}));
    mock.push_tool_call("run_command", json!({"command": "echo hi"}));
    mock.push_text("fin");
    let mut a = agent(&mock, ctx, PermissionMode::AutoProject);
    let evs = run(&mut a, "x", PermissionReply::AllowOnce).await;
    assert_eq!(std::fs::read_to_string(d.path().join("x.txt")).unwrap(), "ok");
    assert_eq!(asked(&evs), 1);
}

#[tokio::test]
async fn dangerous_command_asks_even_in_auto_all() {
    let (d, mock, ctx) = setup();
    std::fs::create_dir(d.path().join("build")).unwrap();
    mock.push_tool_call("run_command", json!({"command": "rm -rf build"}));
    mock.push_text("fin");
    let mut a = agent(&mock, ctx, PermissionMode::AutoAll);
    let evs = run(&mut a, "limpia", PermissionReply::Deny { feedback: None }).await;
    let req = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequest(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert!(req.dangerous);
    assert!(d.path().join("build").exists());
}

#[tokio::test]
async fn allow_session_skips_next_prompt() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("run_command", json!({"command": "echo 1"}));
    mock.push_tool_call("run_command", json!({"command": "echo 2"}));
    mock.push_text("fin");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "x", PermissionReply::AllowSession).await;
    assert_eq!(asked(&evs), 1);
}

#[tokio::test]
async fn outside_project_read_requires_permission() {
    let (_d, mock, ctx) = setup();
    let outside = tempfile::tempdir().unwrap();
    let f = outside.path().join("fuera.txt");
    std::fs::write(&f, "dato externo").unwrap();
    mock.push_tool_call("read_file", json!({"path": f.to_str().unwrap()}));
    mock.push_text("ok");
    let mut a = agent(&mock, ctx.clone(), PermissionMode::AutoAll);
    let evs = run(&mut a, "lee", PermissionReply::AllowOnce).await;
    assert_eq!(asked(&evs), 1);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(o), .. } if o.content.contains("dato externo"))));

    // denegado: no se lee
    mock.push_tool_call("read_file", json!({"path": f.to_str().unwrap()}));
    let mut a = agent(&mock, ctx, PermissionMode::AutoAll);
    let evs = run(&mut a, "lee", PermissionReply::Deny { feedback: None }).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(_), .. })));
}

#[tokio::test]
async fn failing_edit_does_not_ask() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("edit_file", json!({"path": "hola.txt", "old_string": "no existe", "new_string": "x"}));
    mock.push_text("perdón");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "x", PermissionReply::AllowOnce).await;
    assert_eq!(asked(&evs), 0);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { result: Err(m), .. } if m.contains("no se encontró"))));
}

#[tokio::test]
async fn undo_restores_files_and_conversation() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("edit_file", json!({"path": "hola.txt", "old_string": "secreto", "new_string": "roto"}));
    mock.push_tool_call("write_file", json!({"path": "nuevo.txt", "content": "n"}));
    mock.push_text("hecho");
    let mut a = agent(&mock, ctx, PermissionMode::AutoProject);
    run(&mut a, "primer turno", PermissionReply::AllowOnce).await;
    mock.push_text("respuesta 2");
    run(&mut a, "segundo turno", PermissionReply::AllowOnce).await;
    assert_eq!(a.messages().len(), 8);

    let rep = a.undo().unwrap();
    assert_eq!(rep.label, "segundo turno");
    assert_eq!(a.messages().len(), 6);
    let rep = a.undo().unwrap();
    assert_eq!(rep.restored.len(), 1);
    assert_eq!(rep.deleted.len(), 1);
    assert_eq!(std::fs::read_to_string(d.path().join("hola.txt")).unwrap(), "contenido secreto\n");
    assert!(!d.path().join("nuevo.txt").exists());
    assert!(a.messages().is_empty());
    assert!(a.undo().is_none());
}

#[tokio::test]
async fn prompt_mode_parses_text_calls_and_hides_them() {
    let (_d, _mock, ctx) = setup();
    let mock = Arc::new(MockProvider::new().without_native_tools());
    mock.push_text("Lo leo. <tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"hola.txt\"}}</tool_call>");
    mock.push_text("Dice contenido secreto");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "lee", PermissionReply::AllowOnce).await;

    let shown: String = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(!shown.contains("tool_call"), "{shown}");
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(o), .. } if o.content.contains("secreto"))));
    let reqs = mock.requests();
    assert!(reqs[0].tools.is_empty(), "en modo prompt no se envían tools nativas");
    assert!(reqs[0].messages[0].content.contains("<tool_call>"));
    // el resultado vuelve como mensaje de usuario <tool_result>
    assert!(reqs[1].messages.iter().any(|m| m.role == Role::User && m.content.contains("<tool_result")));
    assert_eq!(a.tool_mode(), ToolMode::Prompt);
}

#[tokio::test]
async fn prompt_mode_retries_malformed_calls() {
    let (_d, _mock, ctx) = setup();
    let mock = Arc::new(MockProvider::new().without_native_tools());
    mock.push_text("<tool_call>esto no es json</tool_call>");
    mock.push_text("<tool_call>{\"name\": \"list_dir\", \"arguments\": {}}</tool_call>");
    mock.push_text("listo");
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let evs = run(&mut a, "lista", PermissionReply::AllowOnce).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(n) if n.contains("mal formada"))));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(_), .. })));
    assert!(mock.requests()[1].messages.iter().any(|m| m.content.contains("no es válida")));
}

#[tokio::test]
async fn unknown_tool_and_max_steps() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("borrar_todo", json!({}));
    mock.push_text("perdón");
    let mut a = agent(&mock, ctx.clone(), PermissionMode::Ask);
    let evs = run(&mut a, "x", PermissionReply::AllowOnce).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolEnd { result: Err(m), .. } if m.contains("desconocida"))));

    for _ in 0..5 {
        mock.push_tool_call("list_dir", json!({}));
    }
    let cfg = AgentConfig { max_steps: 3, ..Default::default() };
    let mut a = Agent::new(mock.clone(), astrodev_tools::default_registry(), "m", ctx, cfg);
    let evs = run(&mut a, "x", PermissionReply::AllowOnce).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Error(m)) if m.contains("máximo")));
}

#[tokio::test]
async fn auto_compaction_summarizes_old_messages() {
    let (_d, mock, ctx) = setup();
    let summarizer = Arc::new(MockProvider::new());
    summarizer.push_text("RESUMEN: el usuario pidió cosas");
    let mut a = agent(&mock, ctx, PermissionMode::Ask).with_context_window(2048).with_summarizer(Some(ModelRef::new(
        "s",
        summarizer.clone(),
        "resumidor",
    )));
    for i in 0..6 {
        mock.push_text(&"bla ".repeat(300));
        run(&mut a, &format!("petición {i} {}", "x".repeat(400)), PermissionReply::AllowOnce).await;
    }
    assert!(!summarizer.requests().is_empty(), "debería haberse usado el resumidor");
    assert!(a.messages()[0].content.contains("RESUMEN"));
    assert!(a.context_tokens() < 2048);
}

#[tokio::test]
async fn fallback_model_is_used_on_error() {
    let (_d, _mock, ctx) = setup();
    struct Failing;
    #[async_trait::async_trait]
    impl astrodev_core::Provider for Failing {
        fn name(&self) -> &str {
            "roto"
        }
        async fn list_models(&self) -> Result<Vec<astrodev_core::provider::ModelInfo>, astrodev_core::ProviderError> {
            Ok(vec![])
        }
        async fn chat_stream(
            &self,
            _: astrodev_core::ChatRequest,
            _: CancellationToken,
        ) -> Result<astrodev_core::provider::EventStream, astrodev_core::ProviderError> {
            Err(astrodev_core::ProviderError::Http { status: 503, body: "caído".into() })
        }
    }
    let backup = Arc::new(MockProvider::new());
    backup.push_text("respuesta del fallback");
    let mut a = Agent::new(Arc::new(Failing), astrodev_tools::default_registry(), "m", ctx, AgentConfig::default())
        .with_fallbacks(vec![ModelRef::new("backup", backup.clone(), "b")]);
    let evs = run(&mut a, "hola", PermissionReply::AllowOnce).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(n) if n.contains("fallback"))));
    assert_eq!(a.messages().last().unwrap().content, "respuesta del fallback");
}

#[tokio::test]
async fn planner_is_used_for_first_step() {
    let (_d, mock, ctx) = setup();
    let planner = Arc::new(MockProvider::new());
    planner.push_tool_call("list_dir", json!({}));
    mock.push_text("ejecutado por el editor");
    let mut a = agent(&mock, ctx, PermissionMode::Ask).with_planner(Some(ModelRef::new("p", planner.clone(), "plan")));
    run(&mut a, "haz algo", PermissionReply::AllowOnce).await;
    assert_eq!(planner.requests().len(), 1);
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn sessions_are_saved_and_resumed() {
    let (_d, mock, ctx) = setup();
    let store_dir = tempfile::tempdir().unwrap();
    mock.push_text("hola!");
    let mut a = agent(&mock, ctx.clone(), PermissionMode::Ask).with_store(SessionStore::new(store_dir.path()));
    run(&mut a, "mi primera sesión", PermissionReply::AllowOnce).await;
    let id = a.session().id.clone();

    let store = SessionStore::new(store_dir.path());
    let list = store.list(Some(ctx.project.root()));
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "mi primera sesión");

    let mut b = agent(&mock, ctx, PermissionMode::Ask);
    b.load_session(store.load(&id).unwrap());
    assert_eq!(b.messages().len(), 2);
}

#[tokio::test]
async fn cancellation_stops_turn() {
    let (_d, mock, ctx) = setup();
    mock.push_script(vec![StreamEvent::TextDelta("a".into()), StreamEvent::Done]);
    let mut a = agent(&mock, ctx, PermissionMode::Ask);
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    cancel.cancel();
    a.run_turn("x", tx, cancel).await;
    let mut last = None;
    while let Ok(e) = rx.try_recv() {
        last = Some(e);
    }
    assert!(matches!(last, Some(AgentEvent::Cancelled)));
}
