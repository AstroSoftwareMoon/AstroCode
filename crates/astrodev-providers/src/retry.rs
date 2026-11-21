//! Reintentos con backoff exponencial y jitter para errores transitorios
//! (conexión, 5xx, 429 respetando `retry-after`).

use std::sync::Arc;
use std::time::Duration;

use astrodev_core::provider::{ChatRequest, EventStream, ModelInfo, Provider, ProviderError};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub struct RetryProvider {
    inner: Arc<dyn Provider>,
    max_retries: u32,
    base_delay: Duration,
}

impl RetryProvider {
    pub fn new(inner: Arc<dyn Provider>) -> Self {
        Self { inner, max_retries: 3, base_delay: Duration::from_millis(800) }
    }

    pub fn with_policy(mut self, max_retries: u32, base_delay: Duration) -> Self {
        self.max_retries = max_retries;
        self.base_delay = base_delay;
        self
    }

    /// Espera para el intento `n` (0-based): base·2ⁿ ± 25 % de jitter, máx. 30 s.
    pub fn delay(&self, attempt: u32, err: &ProviderError) -> Duration {
        if let ProviderError::RateLimited { retry_after_secs: Some(s) } = err {
            return Duration::from_secs((*s).min(60));
        }
        let base = self.base_delay.as_millis() as f64 * 2f64.powi(attempt as i32);
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let jitter = 0.75 + (nanos % 1000) as f64 / 2000.0; // 0.75..1.25
        Duration::from_millis((base * jitter).min(30_000.0) as u64)
    }
}

#[async_trait]
impl Provider for RetryProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn supports_native_tools(&self, model: &str) -> Option<bool> {
        self.inner.supports_native_tools(model)
    }

    async fn probe_native_tools(&self, model: &str) -> Option<bool> {
        self.inner.probe_native_tools(model).await
    }

    async fn context_window(&self, model: &str) -> Option<u32> {
        self.inner.context_window(model).await
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let mut attempt = 0;
        loop {
            match self.inner.list_models().await {
                Err(e) if e.is_retryable() && attempt < self.max_retries => {
                    tokio::time::sleep(self.delay(attempt, &e)).await;
                    attempt += 1;
                }
                r => return r,
            }
        }
    }

    async fn chat_stream(&self, request: ChatRequest, cancel: CancellationToken) -> Result<EventStream, ProviderError> {
        let mut attempt = 0;
        loop {
            match self.inner.chat_stream(request.clone(), cancel.clone()).await {
                Err(e) if e.is_retryable() && attempt < self.max_retries => {
                    let d = self.delay(attempt, &e);
                    tracing::info!("{}: {e}; reintento {} en {:?}", self.inner.name(), attempt + 1, d);
                    tokio::select! {
                        _ = tokio::time::sleep(d) => {}
                        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                    }
                    attempt += 1;
                }
                r => return r,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Flaky {
        fails: AtomicU32,
        status: u16,
    }

    #[async_trait]
    impl Provider for Flaky {
        fn name(&self) -> &str {
            "flaky"
        }
        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            Ok(vec![])
        }
        async fn chat_stream(&self, _: ChatRequest, _: CancellationToken) -> Result<EventStream, ProviderError> {
            if self.fails.fetch_sub(1, Ordering::SeqCst) > 0 {
                Err(ProviderError::Http { status: self.status, body: "x".into() })
            } else {
                Ok(Box::pin(futures::stream::empty()))
            }
        }
    }

    #[tokio::test]
    async fn retries_transient_errors() {
        let p = RetryProvider::new(Arc::new(Flaky { fails: AtomicU32::new(2), status: 503 }))
            .with_policy(3, Duration::from_millis(1));
        assert!(p.chat_stream(ChatRequest::default(), CancellationToken::new()).await.is_ok());
    }

    #[tokio::test]
    async fn gives_up_and_skips_client_errors() {
        let p = RetryProvider::new(Arc::new(Flaky { fails: AtomicU32::new(10), status: 503 }))
            .with_policy(2, Duration::from_millis(1));
        assert!(p.chat_stream(ChatRequest::default(), CancellationToken::new()).await.is_err());
        let inner = Arc::new(Flaky { fails: AtomicU32::new(1), status: 400 });
        let p = RetryProvider::new(inner.clone()).with_policy(3, Duration::from_millis(1));
        assert!(p.chat_stream(ChatRequest::default(), CancellationToken::new()).await.is_err());
        assert_eq!(inner.fails.load(Ordering::SeqCst), 0, "un 400 no se reintenta");
    }

    #[test]
    fn backoff_grows_and_honors_retry_after() {
        let p = RetryProvider::new(Arc::new(Flaky { fails: AtomicU32::new(0), status: 500 }));
        let e = ProviderError::Http { status: 500, body: String::new() };
        assert!(p.delay(3, &e) > p.delay(0, &e));
        assert_eq!(p.delay(0, &ProviderError::RateLimited { retry_after_secs: Some(9) }), Duration::from_secs(9));
    }
}
