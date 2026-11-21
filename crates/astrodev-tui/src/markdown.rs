//! Render ligero de Markdown a líneas de ratatui (encabezados, código, listas, diffs).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            let lang = trimmed.trim_start_matches('`').trim();
            if in_code {
                out.push(Line::from(Span::styled(
                    format!("┌─ {}", if lang.is_empty() { "código" } else { lang }),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                out.push(Line::from(Span::styled(
                    "└─",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }
        if in_code {
            out.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::styled(raw.to_string(), Style::default().fg(Color::LightYellow)),
            ]));
            continue;
        }
        if let Some(h) = trimmed.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            out.push(Line::from(Span::styled(
                h.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        out.push(Line::from(inline(raw)));
    }
    out
}

/// `código` en línea y **negrita**.
fn inline(s: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    let mut code = false;
    let mut bold = false;
    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, code: bool, bold: bool| {
        if buf.is_empty() {
            return;
        }
        let mut st = Style::default();
        if code {
            st = st.fg(Color::LightYellow);
        }
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(std::mem::take(buf), st));
    };
    while let Some(c) = chars.next() {
        if c == '`' {
            flush(&mut buf, &mut spans, code, bold);
            code = !code;
        } else if c == '*' && chars.peek() == Some(&'*') && !code {
            chars.next();
            flush(&mut buf, &mut spans, code, bold);
            bold = !bold;
        } else {
            buf.push(c);
        }
    }
    flush(&mut buf, &mut spans, code, bold);
    spans
}

/// Colorea un diff unificado.
pub fn diff_lines(diff: &str, max: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let total = diff.lines().count();
    for l in diff.lines().take(max) {
        let st = if l.starts_with("+++") || l.starts_with("---") {
            Style::default().fg(Color::DarkGray)
        } else if l.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if l.starts_with('-') {
            Style::default().fg(Color::Red)
        } else if l.starts_with("@@") {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        out.push(Line::from(Span::styled(format!("    {l}"), st)));
    }
    if total > max {
        out.push(Line::from(Span::styled(
            format!("    … {} líneas más", total - max),
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_blocks_and_inline() {
        let lines = render("# Título\ntexto `x` **b**\n```rust\nfn a(){}\n```");
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].spans[0].content, "Título");
        assert_eq!(lines[1].spans.len(), 4);
        assert!(lines[3].spans[1].content.contains("fn a"));
    }

    #[test]
    fn diff_truncates() {
        let d = "+a\n-b\n c\n+d";
        assert_eq!(diff_lines(d, 2).len(), 3);
    }
}
