//! Decodificador incremental de Server-Sent Events.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Algoritmo estándar: las líneas `data:` se acumulan y una línea vacía cierra el evento.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn flush(&mut self, out: &mut Vec<SseEvent>) {
        if !self.data.is_empty() {
            out.push(SseEvent { event: self.event.take(), data: self.data.join("\n") });
            self.data.clear();
        }
        self.event = None;
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.flush(out);
        } else if line.starts_with(':') {
            // comentario / keep-alive
        } else if let Some(e) = line.strip_prefix("event:") {
            self.event = Some(e.trim().to_string());
        } else if let Some(d) = line.strip_prefix("data:") {
            self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
        }
    }

    /// Procesa bytes y devuelve los eventos completos.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]).trim_end_matches('\r').to_string();
            self.line(&line, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).trim_end().to_string();
            self.line(&rest, &mut out);
        }
        self.flush(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_events_across_chunks() {
        let mut d = SseDecoder::new();
        let mut evs = d.push(b"event: message_start\ndata: {\"a\":");
        evs.extend(d.push(b"1}\n"));
        evs.extend(d.push(b"\nevent: ping\r\ndata: {}\r\n\r\n: comentario\n"));
        evs.extend(d.push(b"data: [DONE]\n\n"));
        evs.extend(d.finish());
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0], SseEvent { event: Some("message_start".into()), data: "{\"a\":1}".into() });
        assert_eq!(evs[1].event.as_deref(), Some("ping"));
        assert_eq!(evs[2].data, "[DONE]");
    }

    #[test]
    fn data_only_stream() {
        let mut d = SseDecoder::new();
        let mut evs = d.push(b"data: {\"x\":1}\n\ndata: {\"x\":2}\n\n");
        evs.extend(d.finish());
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].data, "{\"x\":2}");
    }
}
