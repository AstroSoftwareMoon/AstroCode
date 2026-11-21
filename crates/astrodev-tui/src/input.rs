//! Campo de entrada multilínea con cursor e historial.

use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct Input {
    text: String,
    /// Posición del cursor en bytes (siempre en frontera de carácter).
    cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_pos = None;
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(i..self.cursor, "");
            self.cursor = i;
        }
    }

    pub fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text.replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    pub fn left(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = i;
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.text.len());
    }

    /// Borra la palabra anterior (Ctrl+W).
    pub fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed.rfind(|c: char| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    pub fn is_multiline(&self) -> bool {
        self.text.contains('\n')
    }

    /// (columna, fila) visual del cursor.
    pub fn cursor_pos(&self) -> (u16, u16) {
        let before = &self.text[..self.cursor];
        let row = before.matches('\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = before[line_start..].width();
        (col as u16, row as u16)
    }

    /// Envía el texto actual y lo guarda en el historial.
    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.history_pos = None;
        if !t.trim().is_empty() && self.history.last() != Some(&t) {
            self.history.push(t.clone());
        }
        t
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        self.text = self.history[pos].clone();
        self.cursor = self.text.len();
    }

    pub fn history_next(&mut self) {
        match self.history_pos {
            None => {}
            Some(p) if p + 1 >= self.history.len() => {
                self.history_pos = None;
                self.text.clear();
                self.cursor = 0;
            }
            Some(p) => {
                self.history_pos = Some(p + 1);
                self.text = self.history[p + 1].clone();
                self.cursor = self.text.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_with_multibyte() {
        let mut i = Input::default();
        for c in "añb".chars() {
            i.insert_char(c);
        }
        i.left();
        i.backspace();
        assert_eq!(i.text(), "ab");
        i.home();
        i.delete();
        assert_eq!(i.text(), "b");
        i.end();
        i.insert_str("\r\nx");
        assert_eq!(i.text(), "b\nx");
        assert_eq!(i.cursor_pos(), (1, 1));
        assert_eq!(i.line_count(), 2);
    }

    #[test]
    fn history_navigation() {
        let mut i = Input::default();
        i.insert_str("uno");
        i.take();
        i.insert_str("dos");
        i.take();
        i.history_prev();
        assert_eq!(i.text(), "dos");
        i.history_prev();
        assert_eq!(i.text(), "uno");
        i.history_next();
        assert_eq!(i.text(), "dos");
        i.history_next();
        assert_eq!(i.text(), "");
    }

    #[test]
    fn delete_word() {
        let mut i = Input::default();
        i.insert_str("hola mundo  ");
        i.delete_word();
        assert_eq!(i.text(), "hola ");
    }
}
