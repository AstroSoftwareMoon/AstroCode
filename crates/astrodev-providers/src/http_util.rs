//! Utilidades compartidas por los proveedores HTTP.

use std::time::Duration;

use astrodev_core::provider::{EventStream, ProviderError, StreamEvent};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::sse::{SseDecoder, SseEvent};

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("astrodev/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("cliente HTTP")
}

pub fn connect_err(provider: &str, url: &str, e: reqwest::Error) -> ProviderError {
    ProviderError::Connect { provider: provider.to_string(), url: url.to_string(), source: Box::new(e) }
}

/// Convierte una respuesta de error HTTP en `ProviderError`.
pub async fn error_from_response(provider: &str, resp: reqwest::Response) -> ProviderError {
    let status = resp.status().as_u16();
    let retry_after =
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let text = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error").filter(|e| e.is_string()))
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.chars().take(500).collect());
    match status {
        401 | 403 => ProviderError::Auth { provider: provider.into(), body: msg },
        404 => ProviderError::ModelNotFound(msg),
        429 => ProviderError::RateLimited { retry_after_secs: retry_after },
        _ => ProviderError::Http { status, body: msg },
    }
}

/// Lanza la lectura de un cuerpo SSE en una tarea y traduce cada evento con `handle`.
/// `handle` recibe el evento y un `Vec` donde dejar los `StreamEvent` resultantes;
/// devuelve `Ok(true)` cuando el stream ha terminado.
pub fn sse_stream<H>(resp: reqwest::Response, cancel: CancellationToken, mut handle: H) -> EventStream
where
    H: FnMut(SseEvent, &mut Vec<StreamEvent>) -> Result<bool, ProviderError> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Result<StreamEvent, ProviderError>>(128);
    let mut bytes = resp.bytes_stream();
    tokio::spawn(async move {
        let mut dec = SseDecoder::new();
        let mut finished = false;
        loop {
            let chunk = tokio::select! {
                c = bytes.next() => c,
                _ = cancel.cancelled() => {
                    let _ = tx.send(Err(ProviderError::Cancelled)).await;
                    return;
                }
            };
            let events = match chunk {
                Some(Ok(b)) => dec.push(&b),
                Some(Err(e)) => {
                    let _ = tx.send(Err(ProviderError::Other(format!("stream cortado: {e}")))).await;
                    return;
                }
                None => {
                    finished = true;
                    dec.finish()
                }
            };
            for ev in events {
                let mut out = Vec::new();
                let done = match handle(ev, &mut out) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
                for o in out {
                    if tx.send(Ok(o)).await.is_err() {
                        return;
                    }
                }
                if done {
                    let _ = tx.send(Ok(StreamEvent::Done)).await;
                    return;
                }
            }
            if finished {
                let _ = tx.send(Ok(StreamEvent::Done)).await;
                return;
            }
        }
    });
    Box::pin(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)))
}

pub fn is_local(url: &str) -> bool {
    let u = url.to_lowercase();
    u.contains("://localhost") || u.contains("://127.") || u.contains("://0.0.0.0") || u.contains("://[::1]")
}
