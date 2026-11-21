//! Ajuste de línea propio que conserva la sangría en las líneas de continuación.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Parte `line` en varias de ancho máximo `width`. Las continuaciones se sangran
/// con el mismo número de columnas que el prefijo inicial de espacios/símbolos de lista.
pub fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width < 8 {
        return vec![line];
    }
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| {
            let st = line.style.patch(s.style);
            s.content.chars().map(move |c| (c, st))
        })
        .collect();
    let total: usize = cells.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
    if total <= width {
        return vec![line];
    }
    let indent = continuation_indent(&cells).min(width / 2);

    let mut out = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < cells.len() {
        let avail = if first { width } else { width - indent };
        // Avanzar hasta llenar el ancho.
        let mut w = 0;
        let mut end = start;
        let mut last_space = None;
        while end < cells.len() {
            let cw = cells[end].0.width().unwrap_or(0);
            if w + cw > avail {
                break;
            }
            if cells[end].0 == ' ' {
                last_space = Some(end);
            }
            w += cw;
            end += 1;
        }
        if end < cells.len() && cells[end].0 != ' ' {
            if let Some(sp) = last_space.filter(|sp| *sp > start) {
                end = sp + 1;
            }
        }
        if end == start {
            end = start + 1; // carácter más ancho que el hueco
        }
        let mut spans = Vec::new();
        if !first && indent > 0 {
            spans.push(Span::raw(" ".repeat(indent)));
        }
        let mut seg = &cells[start..end];
        if !first {
            while seg.first().is_some_and(|(c, _)| *c == ' ') {
                seg = &seg[1..];
            }
        }
        // Agrupar por estilo.
        let mut buf = String::new();
        let mut cur: Option<Style> = None;
        for (c, st) in seg {
            if cur.is_some_and(|s| s != *st) {
                spans.push(Span::styled(std::mem::take(&mut buf), cur.unwrap()));
            }
            cur = Some(*st);
            buf.push(*c);
        }
        if let Some(st) = cur {
            let trimmed = buf.trim_end().to_string();
            spans.push(Span::styled(trimmed, st));
        }
        out.push(Line::from(spans));
        start = end;
        first = false;
    }
    out
}

fn continuation_indent(cells: &[(char, Style)]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < cells.len() && cells[i].0 == ' ' {
        n += 1;
        i += 1;
    }
    // Prefijos de lista / marcadores: "- ", "* ", "› ", "✓ ", "1. "
    let rest: String = cells[i..].iter().take(4).map(|(c, _)| *c).collect();
    for p in ["- ", "* ", "› ", "✓ ", "✗ ", "∴ ", "• "] {
        if rest.starts_with(p) {
            return n + p.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>();
        }
    }
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && rest[digits..].starts_with(". ") {
        return n + digits + 2;
    }
    n
}

pub fn wrap_all(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    lines.into_iter().flat_map(|l| wrap_line(l, width)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn keeps_indent() {
        let l = Line::from("  - uno dos tres cuatro cinco seis siete");
        let w = wrap_line(l, 16);
        assert!(w.len() > 1);
        assert_eq!(text(&w[0]), "  - uno dos tres");
        assert!(text(&w[1]).starts_with("    cuatro"));
        for x in &w {
            assert!(unicode_width::UnicodeWidthStr::width(text(x).as_str()) <= 16);
        }
    }

    #[test]
    fn short_lines_untouched_and_long_words_split() {
        assert_eq!(wrap_line(Line::from("hola"), 20).len(), 1);
        let w = wrap_line(Line::from("x".repeat(30)), 10);
        assert_eq!(w.len(), 3);
    }
}
