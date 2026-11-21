//! Test de humo tras instalar: un prompt corto, medición de tok/s y ajuste automático
//! del contexto para que el modelo quede entero en GPU.

use std::sync::Arc;
use std::time::Instant;

use astrodev_core::provider::{ChatRequest, Provider, StreamEvent};
use astrodev_core::Message;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::ollama::OllamaClient;
use crate::{InstallError, Reporter, Result};

pub const PROMPT: &str = "Responde únicamente con la palabra OK.";

#[derive(Debug, Clone, PartialEq)]
pub struct SmokeResult {
    pub reply: String,
    pub tokens_per_second: f64,
    pub first_token_ms: u128,
    pub output_tokens: u64,
}

impl SmokeResult {
    pub fn looks_ok(&self) -> bool {
        !self.reply.trim().is_empty()
    }
}

pub async fn smoke_test(
    provider: &Arc<dyn Provider>,
    model: &str,
    ctx: u32,
    cancel: &CancellationToken,
) -> Result<SmokeResult> {
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::user(PROMPT)],
        tools: Vec::new(),
        temperature: Some(0.0),
        max_tokens: Some(256),
        context_window: Some(ctx),
    };
    let start = Instant::now();
    let mut stream = provider
        .chat_stream(req, cancel.clone())
        .await
        .map_err(|e| InstallError::Other(format!("el test de humo falló: {e}")))?;
    let mut reply = String::new();
    let mut first: Option<Instant> = None;
    let mut out_tokens = 0u64;
    let mut reported_tps = None;
    let mut deltas = 0u64;
    while let Some(ev) = stream.next().await {
        match ev.map_err(|e| InstallError::Other(format!("el test de humo falló: {e}")))? {
            StreamEvent::TextDelta(t) | StreamEvent::ThinkingDelta(t) => {
                first.get_or_insert_with(Instant::now);
                deltas += 1;
                if !t.is_empty() {
                    reply.push_str(&t);
                }
            }
            StreamEvent::Usage(u) => out_tokens += u.output_tokens,
            StreamEvent::Stats { tokens_per_second } => reported_tps = Some(tokens_per_second),
            StreamEvent::Done => break,
            StreamEvent::ToolCall(_) => {}
        }
    }
    let first_token_ms = first.map(|f| (f - start).as_millis()).unwrap_or(0);
    let tokens = if out_tokens > 0 { out_tokens } else { deltas };
    let gen_secs = first.map(|f| f.elapsed().as_secs_f64()).unwrap_or(0.0).max(0.001);
    let tps = reported_tps.unwrap_or(tokens as f64 / gen_secs);
    Ok(SmokeResult { reply: reply.trim().to_string(), tokens_per_second: tps, first_token_ms, output_tokens: tokens })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuneResult {
    pub ctx: u32,
    pub fully_on_gpu: bool,
    pub smoke: SmokeResult,
}

/// Prueba contextos de mayor a menor y se queda con el primero que deja el modelo
/// entero en GPU (según `/api/ps`). Sin GPU, el primero que funcione.
pub async fn tune_ollama(
    client: &OllamaClient,
    provider: &Arc<dyn Provider>,
    model: &str,
    candidates: &[u32],
    has_gpu: bool,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<TuneResult> {
    let mut last: Option<TuneResult> = None;
    for &ctx in candidates {
        rep.step(format!("Test de humo con contexto {ctx}"));
        let smoke = smoke_test(provider, model, ctx, cancel).await?;
        let loaded = client.ps().await.unwrap_or_default();
        let on_gpu = loaded
            .iter()
            .find(|m| m.name == model || m.name.starts_with(&format!("{model}:")) || model.starts_with(&m.name))
            .map(|m| m.fully_on_gpu())
            .unwrap_or(!has_gpu);
        rep.log(format!(
            "  ctx {ctx}: «{}» · {:.1} tok/s · {}",
            smoke.reply.chars().take(30).collect::<String>(),
            smoke.tokens_per_second,
            if !has_gpu {
                "CPU"
            } else if on_gpu {
                "100 % en GPU"
            } else {
                "parte en CPU"
            }
        ));
        let r = TuneResult { ctx, fully_on_gpu: on_gpu, smoke };
        if !has_gpu || on_gpu {
            return Ok(r);
        }
        client.unload(model).await;
        last = Some(r);
    }
    last.ok_or_else(|| InstallError::Other("sin contextos que probar".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_providers::MockProvider;

    #[tokio::test]
    async fn smoke_measures_speed() {
        let mock = MockProvider::new();
        mock.push_script(vec![
            StreamEvent::TextDelta("OK".into()),
            StreamEvent::Usage(astrodev_core::Usage { input_tokens: 10, output_tokens: 1 }),
            StreamEvent::Stats { tokens_per_second: 42.0 },
            StreamEvent::Done,
        ]);
        let p: Arc<dyn Provider> = Arc::new(mock);
        let r = smoke_test(&p, "m", 4096, &CancellationToken::new()).await.unwrap();
        assert_eq!(r.reply, "OK");
        assert_eq!(r.tokens_per_second, 42.0);
        assert!(r.looks_ok());
    }

    #[tokio::test]
    async fn tune_falls_back_to_smaller_context() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // Primer /api/ps: parte en CPU; segundo: todo en GPU.
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"models":[{"name":"m:latest","size":100,"size_vram":50}]}"#),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"models":[{"name":"m:latest","size":100,"size_vram":100}]}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/generate"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let mock = MockProvider::new();
        mock.push_text("OK");
        mock.push_text("OK");
        let p: Arc<dyn Provider> = Arc::new(mock);
        let client = OllamaClient::new(&server.uri());
        let r = tune_ollama(&client, &p, "m", &[16384, 8192], true, &Reporter::silent(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.ctx, 8192);
        assert!(r.fully_on_gpu);
    }
}
