//! Integración: bucle agente + proveedor Anthropic (servidor simulado).

use std::sync::Arc;

use astrodev_core::config::{AgentConfig, PermissionMode, PermissionsConfig};
use astrodev_core::permissions::Permissions;
use astrodev_core::{Agent, AgentEvent, Project, ToolContext};
use astrodev_providers::anthropic::AnthropicProvider;
use astrodev_providers::keys::KeySource;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn sse(events: &[Value]) -> String {
    events.iter().map(|d| format!("event: {}\ndata: {d}\n\n", d["type"].as_str().unwrap())).collect()
}

struct Script;
impl Respond for Script {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let last = body["messages"].as_array().unwrap().last().unwrap().clone();
        let has_result = last["content"].as_array().is_some_and(|c| c.iter().any(|b| b["type"] == "tool_result"));
        let events = if !has_result {
            vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":30}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"FIRMA"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_A","name":"read_file","input":{}}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"nota.txt\"}"}}),
                json!({"type":"content_block_stop","index":1}),
                json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),
                json!({"type":"message_stop"}),
            ]
        } else {
            vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":60}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"La nota dice hola."}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
                json!({"type":"message_stop"}),
            ]
        };
        ResponseTemplate::new(200).set_body_string(sse(&events))
    }
}

#[tokio::test]
async fn tool_loop_replays_signed_thinking_and_tool_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/messages")).respond_with(Script).mount(&server).await;

    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("nota.txt"), "hola").unwrap();
    let ctx = ToolContext::new(Arc::new(Project::new(d.path()).unwrap()));
    let provider =
        Arc::new(AnthropicProvider::new("anthropic", Some(&server.uri()), KeySource::fixed(Some("k".into())), None));
    let mut agent =
        Agent::new(provider, astrodev_tools::default_registry(), "claude-sonnet-5-5", ctx, AgentConfig::default())
            .with_permissions(Permissions::new(&PermissionsConfig { mode: PermissionMode::Ask, ..Default::default() }));

    let (tx, mut rx) = mpsc::channel(256);
    agent.run_turn("¿qué dice nota.txt?", tx, CancellationToken::new()).await;
    let mut evs = Vec::new();
    while let Ok(e) = rx.try_recv() {
        evs.push(e);
    }
    assert!(matches!(evs.last(), Some(AgentEvent::TurnDone)), "{evs:?}");
    assert_eq!(agent.messages().last().unwrap().content, "La nota dice hola.");

    let reqs: Vec<Request> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::POST)
        .collect();
    assert_eq!(reqs.len(), 2);
    let second: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let msgs = second["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["content"][0]["signature"], "FIRMA");
    assert_eq!(msgs[1]["content"][1]["id"], "toolu_A");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_A");
    assert!(msgs[2]["content"][0]["content"].as_str().unwrap().contains("hola"));
    assert!(second.get("temperature").is_none());
    assert_eq!(agent.usage().input_tokens, 90);
}
