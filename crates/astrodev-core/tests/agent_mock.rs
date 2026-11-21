//! Integración: bucle agente + herramientas reales + proveedor falso.

use std::sync::Arc;

use astrodev_core::agent::ToolPolicy;
use astrodev_core::config::AgentConfig;
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

async fn run(agent: &mut Agent, input: &str) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(256);
    agent.run_turn(input, tx, CancellationToken::new()).await;
    let mut evs = Vec::new();
    while let Ok(e) = rx.try_recv() {
        evs.push(e);
    }
    evs
}

#[tokio::test]
async fn tool_loop_reads_file_and_answers() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("read_file", json!({"path": "hola.txt"}));
    mock.push_text("El archivo dice: contenido secreto");
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        AgentConfig::default(),
    );

    let evs = run(&mut agent, "¿qué hay en hola.txt?").await;

    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolStart(c) if c.name == "read_file")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolEnd { result: Ok(o), .. } if o.content.contains("contenido secreto"))));
    assert!(matches!(evs.last(), Some(AgentEvent::TurnDone)));

    // user, assistant(tool_call), tool, assistant(final)
    let roles: Vec<Role> = agent.messages().iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );

    // La segunda petición incluye el resultado de la herramienta; el system prompt incluye ASTRO.md.
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].messages[0].content.contains("Usa siempre tabs."));
    assert!(reqs[1]
        .messages
        .iter()
        .any(|m| m.role == Role::Tool && m.content.contains("contenido secreto")));
    // Política sólo-lectura: no se anuncian herramientas de escritura.
    assert!(reqs[0]
        .tools
        .iter()
        .all(|t| t.name != "write_file" && t.name != "run_command"));
}

#[tokio::test]
async fn write_tools_blocked_in_read_only() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("write_file", json!({"path": "x.txt", "content": "pwned"}));
    mock.push_text("vale");
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        AgentConfig::default(),
    );
    let evs = run(&mut agent, "escribe").await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolEnd { result: Err(m), .. } if m.contains("no está permitida"))));
    assert!(!d.path().join("x.txt").exists());
}

#[tokio::test]
async fn policy_all_allows_writes() {
    let (d, mock, ctx) = setup();
    mock.push_tool_call("write_file", json!({"path": "x.txt", "content": "ok"}));
    mock.push_text("hecho");
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        AgentConfig::default(),
    )
    .with_policy(ToolPolicy::All);
    run(&mut agent, "escribe").await;
    assert_eq!(
        std::fs::read_to_string(d.path().join("x.txt")).unwrap(),
        "ok"
    );
}

#[tokio::test]
async fn unknown_tool_reports_error_and_continues() {
    let (_d, mock, ctx) = setup();
    mock.push_tool_call("borrar_todo", json!({}));
    mock.push_text("perdón");
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        AgentConfig::default(),
    );
    let evs = run(&mut agent, "x").await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolEnd { result: Err(m), .. } if m.contains("desconocida"))
    ));
    assert!(matches!(evs.last(), Some(AgentEvent::TurnDone)));
}

#[tokio::test]
async fn max_steps_is_enforced() {
    let (_d, mock, ctx) = setup();
    for _ in 0..5 {
        mock.push_tool_call("list_dir", json!({}));
    }
    let cfg = AgentConfig {
        max_steps: 3,
        ..Default::default()
    };
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        cfg,
    );
    let evs = run(&mut agent, "x").await;
    assert!(matches!(evs.last(), Some(AgentEvent::Error(m)) if m.contains("máximo")));
    assert_eq!(mock.requests().len(), 3);
}

#[tokio::test]
async fn cancellation_stops_turn() {
    let (_d, mock, ctx) = setup();
    mock.push_text("no debería verse");
    let mut agent = Agent::new(
        mock.clone(),
        astrodev_tools::default_registry(),
        "m",
        ctx,
        AgentConfig::default(),
    );
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    cancel.cancel();
    agent.run_turn("x", tx, cancel).await;
    let mut last = None;
    while let Ok(e) = rx.try_recv() {
        last = Some(e);
    }
    assert!(matches!(last, Some(AgentEvent::Cancelled)));
}
