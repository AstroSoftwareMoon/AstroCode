//! Dibujo de la interfaz.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Entry, ToolStatus};
use crate::markdown;

const ACCENT: Color = Color::Rgb(0x9B, 0x5C, 0xFF);
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const LOGO: &[&str] = &[
    r"    _        _             ____             ",
    r"   / \   ___| |_ _ __ ___ |  _ \  _____   __",
    r"  / _ \ / __| __| '__/ _ \| | | |/ _ \ \ / /",
    r" / ___ \\__ \ |_| | | (_) | |_| |  __/\ V / ",
    r"/_/   \_\___/\__|_|  \___/|____/ \___| \_/  ",
];

pub fn draw(f: &mut Frame, app: &mut App) {
    let input_lines = app.input.line_count().clamp(1, 8) as u16;
    let [header, chat, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(input_lines + 2),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_chat(f, app, chat);
    draw_input(f, app, input);
    draw_status(f, app, status);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let model = if app.model.is_empty() {
        "(sin modelo)"
    } else {
        &app.model
    };
    let line = Line::from(vec![
        Span::styled(
            " ✦ AstroDev ",
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{}/{}", app.provider_name, model),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.project.clone(), Style::default().fg(Color::Gray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn entry_lines(app: &App, e: &Entry, out: &mut Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    match e {
        Entry::Welcome => {
            for l in LOGO {
                out.push(gradient_line(l));
            }
            out.push(Line::from(""));
            out.push(Line::from(vec![
                Span::raw("Agente de programación local-first. Escribe tu petición o "),
                Span::styled("/help", Style::default().fg(Color::Cyan)),
                Span::raw("."),
            ]));
            if app.has_astro_md {
                out.push(Line::from(Span::styled(
                    "✓ ASTRO.md cargado como contexto del proyecto",
                    Style::default().fg(Color::Green),
                )));
            } else {
                out.push(Line::from(Span::styled(
                    "Consejo: crea un ASTRO.md con instrucciones para este proyecto.",
                    dim,
                )));
            }
        }
        Entry::User(t) => {
            for (i, l) in t.split('\n').enumerate() {
                let prefix = if i == 0 { "› " } else { "  " };
                out.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(l.to_string(), Style::default().add_modifier(Modifier::BOLD)),
                ]));
            }
        }
        Entry::Assistant { text, thinking } => {
            if !thinking.trim().is_empty() {
                let t: String = thinking
                    .trim()
                    .chars()
                    .rev()
                    .take(300)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                for l in t.lines() {
                    out.push(Line::from(Span::styled(
                        format!("  ∴ {l}"),
                        dim.add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            for l in markdown::render(text.trim_end()) {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(l.spans);
                out.push(Line::from(spans));
            }
        }
        Entry::Tool { call, status } => {
            let summary = summarize_args(&call.arguments);
            let (icon, color) = match status {
                ToolStatus::Running => (SPINNER[app.spinner % SPINNER.len()], Color::Yellow),
                ToolStatus::Ok { .. } => ("✓", Color::Green),
                ToolStatus::Failed(_) => ("✗", Color::Red),
            };
            out.push(Line::from(vec![
                Span::styled(format!("  {icon} "), Style::default().fg(color)),
                Span::styled(
                    call.name.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {summary}"), Style::default().fg(Color::Gray)),
            ]));
            match status {
                ToolStatus::Running => {}
                ToolStatus::Ok { summary, diff } => {
                    if let Some(d) = diff {
                        out.extend(markdown::diff_lines(d, 40));
                    } else {
                        let n = summary.lines().count();
                        for l in summary.lines().take(4) {
                            let l: String = l.chars().take(160).collect();
                            out.push(Line::from(Span::styled(format!("    {l}"), dim)));
                        }
                        if n > 4 {
                            out.push(Line::from(Span::styled(
                                format!("    … {} líneas más", n - 4),
                                dim,
                            )));
                        }
                    }
                }
                ToolStatus::Failed(e) => {
                    out.push(Line::from(Span::styled(
                        format!("    {e}"),
                        Style::default().fg(Color::Red),
                    )));
                }
            }
        }
        Entry::Info(t) => {
            for l in t.split('\n') {
                out.push(Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(Color::Blue),
                )));
            }
        }
        Entry::Error(t) => {
            for (i, l) in t.split('\n').enumerate() {
                let p = if i == 0 { "  ✗ " } else { "    " };
                out.push(Line::from(Span::styled(
                    format!("{p}{l}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }
}

/// Colores de la marca: magenta → violeta → cian (igual que assets/logo.svg).
const BRAND: [(u8, u8, u8); 3] = [(0xFF, 0x4F, 0xD8), (0x9B, 0x5C, 0xFF), (0x3E, 0xC8, 0xFF)];

fn brand_color(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b, u) = if t < 0.55 {
        (BRAND[0], BRAND[1], t / 0.55)
    } else {
        (BRAND[1], BRAND[2], (t - 0.55) / 0.45)
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn gradient_line(s: &str) -> Line<'static> {
    let n = s.chars().count().max(1) as f32;
    Line::from(
        s.chars()
            .enumerate()
            .map(|(i, c)| {
                Span::styled(
                    c.to_string(),
                    Style::default()
                        .fg(brand_color(i as f32 / n))
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    )
}

/// Resumen corto de los argumentos para mostrar en una línea.
pub fn summarize_args(args: &serde_json::Value) -> String {
    let pick = ["path", "pattern", "command", "args"];
    for k in pick {
        if let Some(v) = args.get(k) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| x.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                other => other.to_string(),
            };
            let s = s.replace('\n', " ");
            return if s.chars().count() > 80 {
                format!("{}…", s.chars().take(80).collect::<String>())
            } else {
                s
            };
        }
    }
    String::new()
}

fn draw_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, e) in app.entries.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        entry_lines(app, e, &mut lines);
    }
    if app.busy
        && !matches!(
            app.entries.last(),
            Some(Entry::Assistant { .. })
                | Some(Entry::Tool {
                    status: ToolStatus::Running,
                    ..
                })
        )
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {} pensando…", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(Color::Yellow),
        )));
    }
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total = p.line_count(inner.width) as u16;
    let max_scroll = total.saturating_sub(inner.height);
    if app.scroll_from_bottom > max_scroll {
        app.scroll_from_bottom = max_scroll;
    }
    let y = max_scroll - app.scroll_from_bottom;
    f.render_widget(p.scroll((y, 0)), inner);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border_color = if app.busy { Color::DarkGray } else { ACCENT };
    let title = if app.busy {
        " Esc para cancelar "
    } else {
        " Mensaje · Enter enviar · Alt+Enter nueva línea "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(Color::DarkGray)));
    let inner = block.inner(area);
    let (cx, cy) = app.input.cursor_pos();
    // Desplazamiento vertical si hay más líneas que espacio.
    let vscroll = cy.saturating_sub(inner.height.saturating_sub(1));
    let hscroll = cx.saturating_sub(inner.width.saturating_sub(1));
    let text = if app.input.is_empty() {
        Paragraph::new(Span::styled(
            "Pregunta algo sobre tu código…",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Paragraph::new(app.input.text().to_string()).scroll((vscroll, hscroll))
    };
    f.render_widget(text.block(block), area);
    f.set_cursor_position(Position::new(
        inner.x + cx - hscroll,
        inner.y + cy - vscroll,
    ));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let state = if app.busy {
        Span::styled(
            format!(" {} trabajando ", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled(" ● listo ", Style::default().fg(Color::Green))
    };
    let mut spans = vec![
        state,
        Span::styled(
            format!(
                "│ tokens ↑{} ↓{} ",
                app.usage.input_tokens, app.usage.output_tokens
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some(t) = app.last_tps {
        spans.push(Span::styled(
            format!("│ {t:.1} tok/s "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::styled(
        "│ /help · Ctrl+C salir",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
