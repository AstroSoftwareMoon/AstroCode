//! Decodificador incremental de líneas (NDJSON / SSE) a partir de trozos de bytes.

#[derive(Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Añade bytes y devuelve las líneas completas (sin `\n`, sin `\r`, no vacías).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line[..line.len() - 1])
                .trim_end_matches('\r')
                .to_string();
            if !s.trim().is_empty() {
                lines.push(s);
            }
        }
        lines
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Resto sin `\n` final (al cerrar el stream).
    pub fn finish(&mut self) -> Option<String> {
        let s = String::from_utf8_lossy(&std::mem::take(&mut self.buf))
            .trim()
            .to_string();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_chunks() {
        let mut d = LineDecoder::new();
        assert!(d.push(b"{\"a\":").is_empty());
        assert_eq!(d.push(b"1}\n{\"b\""), vec!["{\"a\":1}"]);
        assert_eq!(d.push(b":2}\r\n\n"), vec!["{\"b\":2}"]);
        assert!(d.push("ñ".as_bytes()[..1].as_ref()).is_empty());
        assert_eq!(d.push(&"ñ".as_bytes()[1..]), Vec::<String>::new());
        assert_eq!(d.finish().as_deref(), Some("ñ"));
    }
}
