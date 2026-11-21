//! Descargas con progreso, reanudación (HTTP Range), verificación SHA-256 y
//! comprobación de espacio libre.

use std::path::{Path, PathBuf};
use std::time::Instant;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{gb, InstallError, Reporter, Result};

/// Margen de espacio que se deja libre además del archivo.
const SPACE_MARGIN: u64 = 512 * 1024 * 1024;

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// SHA-256 de un archivo (hex en minúsculas).
pub async fn sha256_file(path: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Hugging Face publica el SHA-256 de los archivos LFS en la cabecera `X-Linked-Etag`.
pub async fn remote_sha256(client: &reqwest::Client, url: &str) -> Option<String> {
    let r = client.head(url).send().await.ok()?;
    for h in ["x-linked-etag", "etag"] {
        if let Some(v) = r.headers().get(h).and_then(|v| v.to_str().ok()) {
            let v = v.trim_matches('"').trim_start_matches("W/").trim_matches('"').to_lowercase();
            if v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(v);
            }
        }
    }
    None
}

/// Descarga `url` en `dest`. Si existe `dest.part`, reanuda. Verifica el hash si se da
/// (o si el servidor lo publica, como Hugging Face).
pub async fn download(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    label: &str,
    rep: &Reporter,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let client = crate::http();
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let expected = match expected_sha256 {
        Some(h) => Some(h.to_lowercase()),
        None => remote_sha256(&client, url).await,
    };
    // ¿Ya descargado y válido?
    if dest.exists() {
        match &expected {
            Some(h) if sha256_file(dest).await? == *h => {
                rep.log(format!("{label}: ya estaba descargado y verificado"));
                return Ok(dest.to_path_buf());
            }
            None => {
                rep.log(format!("{label}: ya existe"));
                return Ok(dest.to_path_buf());
            }
            _ => tokio::fs::remove_file(dest).await?,
        }
    }

    let part = part_path(dest);
    let mut have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = tokio::select! {
        r = req.send() => r.map_err(|e| InstallError::Network(e.to_string()))?,
        _ = cancel.cancelled() => return Err(InstallError::Cancelled),
    };
    let status = resp.status().as_u16();
    if status == 416 {
        // Rango no satisfacible: el .part ya está completo (o corrupto); se verifica abajo.
    } else if !(200..300).contains(&status) {
        return Err(InstallError::Http { status, url: url.to_string() });
    }
    let resumed = status == 206;
    if !resumed && status != 416 && have > 0 {
        rep.log(format!("{label}: el servidor no admite reanudar; empiezo de cero"));
        have = 0;
    }
    let remaining = if status == 416 { 0 } else { resp.content_length().unwrap_or(0) };
    let total =
        if status == 416 { Some(have) } else { resp.content_length().map(|l| l + if resumed { have } else { 0 }) };

    // Espacio libre.
    let free = crate::paths::free_space(dest.parent().unwrap_or(Path::new(".")));
    if free > 0 && remaining + SPACE_MARGIN > free {
        return Err(InstallError::NoSpace { needed: gb(remaining + SPACE_MARGIN), available: gb(free) });
    }

    if status != 416 {
        let mut file = if resumed {
            rep.log(format!("{label}: reanudando desde {}", gb(have)));
            tokio::fs::OpenOptions::new().append(true).open(&part).await?
        } else {
            tokio::fs::File::create(&part).await?
        };
        let mut stream = resp.bytes_stream();
        let mut done = have;
        let start = Instant::now();
        let mut last = Instant::now();
        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.cancelled() => {
                    file.flush().await?;
                    rep.log(format!("{label}: cancelado; se podrá reanudar ({} descargados)", gb(done)));
                    return Err(InstallError::Cancelled);
                }
            };
            let Some(chunk) = chunk else { break };
            let chunk =
                chunk.map_err(|e| InstallError::Network(format!("descarga interrumpida (se puede reanudar): {e}")))?;
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            if last.elapsed().as_millis() >= 200 {
                let secs = start.elapsed().as_secs_f64().max(0.001);
                rep.progress(label, done, total, (done - have) as f64 / secs);
                last = Instant::now();
            }
        }
        file.flush().await?;
        rep.progress(label, done, total, 0.0);
        if let Some(t) = total {
            if done < t {
                return Err(InstallError::Network(format!(
                    "descarga incompleta ({done} de {t} bytes); vuelve a intentarlo para reanudar"
                )));
            }
        }
    }

    if let Some(h) = &expected {
        rep.log(format!("{label}: verificando SHA-256…"));
        let actual = sha256_file(&part).await?;
        if actual != *h {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(InstallError::HashMismatch { expected: h.clone(), actual });
        }
        rep.log(format!("{label}: hash verificado ✓"));
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(dest.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn sha(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    /// Responde a Range con 206 y el trozo pedido.
    struct RangeResponder(Vec<u8>);
    impl Respond for RangeResponder {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            if let Some(r) = req.headers.get("range") {
                let s = r.to_str().unwrap().trim_start_matches("bytes=").trim_end_matches('-');
                let from: usize = s.parse().unwrap();
                if from >= self.0.len() {
                    return ResponseTemplate::new(416);
                }
                return ResponseTemplate::new(206).set_body_bytes(self.0[from..].to_vec());
            }
            ResponseTemplate::new(200).set_body_bytes(self.0.clone())
        }
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let server = MockServer::start().await;
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        Mock::given(method("GET")).respond_with(RangeResponder(data.clone())).mount(&server).await;
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("m.gguf");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let p = download(&server.uri(), &dest, Some(&sha(&data)), "m", &Reporter::new(tx), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(p).unwrap(), data);
        assert!(!part_path(&dest).exists());
        let mut saw_progress = false;
        while let Ok(e) = rx.try_recv() {
            saw_progress |= matches!(e, crate::InstallEvent::Progress { .. });
        }
        assert!(saw_progress);
    }

    #[tokio::test]
    async fn resumes_partial_download() {
        let server = MockServer::start().await;
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 7) as u8).collect();
        Mock::given(method("GET"))
            .and(header("range", "bytes=20000-"))
            .respond_with(RangeResponder(data.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("m.bin");
        std::fs::write(part_path(&dest), &data[..20000]).unwrap();
        download(&server.uri(), &dest, Some(&sha(&data)), "m", &Reporter::silent(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[tokio::test]
    async fn hash_mismatch_deletes_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(RangeResponder(b"hola".to_vec())).mount(&server).await;
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("x");
        let e =
            download(&server.uri(), &dest, Some(&"0".repeat(64)), "x", &Reporter::silent(), &CancellationToken::new())
                .await
                .unwrap_err();
        assert!(matches!(e, InstallError::HashMismatch { .. }));
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    #[tokio::test]
    async fn uses_hf_linked_etag() {
        let server = MockServer::start().await;
        let data = b"contenido del modelo".to_vec();
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(200).insert_header("x-linked-etag", format!("\"{}\"", sha(&data))))
            .mount(&server)
            .await;
        let client = crate::http();
        assert_eq!(remote_sha256(&client, &server.uri()).await, Some(sha(&data)));
    }

    #[tokio::test]
    async fn http_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
        let d = tempfile::tempdir().unwrap();
        let e = download(&server.uri(), &d.path().join("x"), None, "x", &Reporter::silent(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(e, InstallError::Http { status: 404, .. }));
    }
}
