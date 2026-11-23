//! Dibujo de la interfaz.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{mode_name, App, Entry, Modal, ToolStatus};
use crate::markdown;
use crate::theme::*;
use crate::wrap::wrap_all;

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
    crate::screens::draw_modal(f, app);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let model = if app.model.is_empty() { "(sin modelo)" } else { &app.model };
    let line = Line::from(vec![
        Span::styled(" ✦ AstroDev ", Style::default().fg(th().on_accent).bg(th().accent).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(format!("{}/{}", app.provider_name, model), Style::default().fg(th().accent3)),
        Span::styled("  ·  ", Style::default().fg(th().dim)),
        Span::styled(app.project.clone(), Style::default().fg(th().muted)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn entry_lines(app: &App, e: &Entry, out: &mut Vec<Line<'static>>) {
    let dim = Style::default().fg(th().dim);
    match e {
        Entry::Welcome => {
            for l in LOGO {
                out.push(gradient_line(l));
            }
            out.push(Line::from(""));
            out.push(Line::from(vec![
                Span::raw("Agente de programación local-first. Escribe tu petición o "),
                Span::styled("/help", Style::default().fg(th().accent3)),
                Span::raw("."),
            ]));
            if !app.memory.is_empty() {
                let list = app.memory.iter().map(|(k, p)| format!("{p} ({k})")).collect::<Vec<_>>().join(" · ");
                out.push(Line::from(Span::styled(format!("✓ Memoria: {list}"), Style::default().fg(th().ok))));
            } else {
                out.push(Line::from(Span::styled(
                    "Consejo: /init crea un ASTRO.md con la memoria del proyecto · # nota para recordar algo",
                    dim,
                )));
            }
        }
        Entry::User(t) => {
            for (i, l) in t.split('\n').enumerate() {
                let prefix = if i == 0 { "› " } else { "  " };
                out.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(th().accent2).add_modifier(Modifier::BOLD)),
                    Span::styled(l.to_string(), Style::default().add_modifier(Modifier::BOLD)),
                ]));
            }
        }
        Entry::Assistant { text, thinking } => {
            if !thinking.trim().is_empty() {
                let t: String = thinking.trim().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
                for l in t.lines() {
                    out.push(Line::from(Span::styled(format!("  ∴ {l}"), dim.add_modifier(Modifier::ITALIC))));
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
                ToolStatus::Running => (SPINNER[app.spinner % SPINNER.len()], th().warn),
                ToolStatus::Ok { .. } => ("✓", th().ok),
                ToolStatus::Failed(_) => ("✗", th().err),
            };
            out.push(Line::from(vec![
                Span::styled(format!("  {icon} "), Style::default().fg(color)),
                Span::styled(call.name.clone(), Style::default().fg(th().accent3).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {summary}"), Style::default().fg(th().muted)),
            ]));
            match status {
                ToolStatus::Running => {}
                ToolStatus::Ok { summary, diff } => {
                    if let Some(d) = diff {
                        out.extend(markdown::diff_lines(d, 40));
                    } else {
                        let n = summary.lines().count();
                        for l in summary.lines().take(4) {
                            let l: String = l.chars().take(200).collect();
                            out.push(Line::from(Span::styled(format!("    {l}"), dim)));
                        }
                        if n > 4 {
                            out.push(Line::from(Span::styled(format!("    … {} líneas más", n - 4), dim)));
                        }
                    }
                }
                ToolStatus::Failed(e) => {
                    for l in e.lines().take(6) {
                        out.push(Line::from(Span::styled(format!("    {l}"), Style::default().fg(th().err))));
                    }
                }
            }
        }
        Entry::Info(t) => {
            for l in t.split('\n') {
                out.push(Line::from(Span::styled(format!("  {l}"), Style::default().fg(th().info))));
            }
        }
        Entry::Error(t) => {
            for (i, l) in t.split('\n').enumerate() {
                let p = if i == 0 { "  ✗ " } else { "    " };
                out.push(Line::from(Span::styled(format!("{p}{l}"), Style::default().fg(th().err))));
            }
        }
    }
}

/// Resumen corto de los argumentos para mostrar en una línea.
pub fn summarize_args(args: &serde_json::Value) -> String {
    for k in ["path", "pattern", "command", "args"] {
        if let Some(v) = args.get(k) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
                other => other.to_string(),
            };
            let s = s.replace('\n', " ");
            return if s.chars().count() > 90 { format!("{}…", s.chars().take(90).collect::<String>()) } else { s };
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
    let waiting = app.busy
        && app.modal.is_none()
        && !matches!(
            app.entries.last(),
            Some(Entry::Assistant { .. }) | Some(Entry::Tool { status: ToolStatus::Running, .. })
        );
    if waiting {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {} pensando…", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(th().warn),
        )));
    }
    let inner = Rect { x: area.x + 1, width: area.width.saturating_sub(2), ..area };
    let lines = wrap_all(lines, inner.width as usize);
    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(inner.height);
    if app.scroll_from_bottom > max_scroll {
        app.scroll_from_bottom = max_scroll;
    }
    let y = max_scroll - app.scroll_from_bottom;
    f.render_widget(Paragraph::new(lines).scroll((y, 0)), inner);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border_color = if app.busy { th().dim } else { th().accent };
    let title = if app.busy {
        " Esc para cancelar ".to_string()
    } else if let Some(c) = crate::commands::complete(app.input.text()) {
        format!(" Tab → /{c} ")
    } else {
        " Mensaje · Enter enviar · Alt+Enter nueva línea · / comandos ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(th().dim)));
    let inner = block.inner(area);
    let (cx, cy) = app.input.cursor_pos();
    let vscroll = cy.saturating_sub(inner.height.saturating_sub(1));
    let hscroll = cx.saturating_sub(inner.width.saturating_sub(1));
    let text = if app.input.is_empty() {
        Paragraph::new(Span::styled("Pregunta algo sobre tu código…", Style::default().fg(th().dim)))
    } else {
        Paragraph::new(app.input.text().to_string()).scroll((vscroll, hscroll))
    };
    f.render_widget(text.block(block), area);
    if app.modal.is_none() {
        f.set_cursor_position(Position::new(inner.x + cx - hscroll, inner.y + cy - vscroll));
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let state = if app.busy {
        Span::styled(format!(" {} trabajando ", SPINNER[app.spinner % SPINNER.len()]), Style::default().fg(th().warn))
    } else {
        Span::styled(" ● listo ", Style::default().fg(th().ok))
    };
    let mode_color = match app.perm_mode {
        astrodev_core::config::PermissionMode::Ask => th().ok,
        astrodev_core::config::PermissionMode::AutoProject => th().warn,
        astrodev_core::config::PermissionMode::AutoAll => th().err,
    };
    let pct = app.context_pct();
    let ctx_color = if pct >= 80 {
        th().err
    } else if pct >= 60 {
        th().warn
    } else {
        th().dim
    };
    let mut spans = vec![
        state,
        Span::styled("│ ", Style::default().fg(th().dim)),
        Span::styled(format!("⏵ {} ", mode_name(app.perm_mode)), Style::default().fg(mode_color)),
        Span::styled("│ ", Style::default().fg(th().dim)),
        Span::styled(format!("ctx {pct}% "), Style::default().fg(ctx_color)),
        Span::styled(
            format!("│ ↑{} ↓{} ", app.usage.input_tokens, app.usage.output_tokens),
            Style::default().fg(th().dim),
        ),
    ];
    if app.cost > 0.0 {
        spans.push(Span::styled(format!("│ ${:.4} ", app.cost), Style::default().fg(th().dim)));
    }
    if let Some(t) = app.last_tps {
        spans.push(Span::styled(format!("│ {t:.1} tok/s "), Style::default().fg(th().dim)));
    }
    spans.push(Span::styled("│ Shift+Tab modo · /help", Style::default().fg(th().dim)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Rectángulo centrado.
pub fn centered(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let w = area.width * w_pct / 100;
    let h = area.height * h_pct / 100;
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub fn modal_block(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)))
}

pub fn draw_permission(f: &mut Frame, app: &App) {
    let Some(Modal::Permission { req, scroll, feedback }) = &app.modal else {
        return;
    };
    let area = centered(f.area(), 88, 80);
    f.render_widget(Clear, area);
    let color = if req.dangerous { th().err } else { th().warn };
    let title = if req.dangerous { "⚠ Confirmación requerida" } else { "Permiso requerido" };
    let block = modal_block(title, color);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [head, body, foot] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(1), Constraint::Length(3)]).areas(inner);

    let detail = match req.call.name.as_str() {
        "run_command" => req.call.arguments.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        _ => summarize_args(&req.call.arguments),
    };
    let head_lines = vec![
        Line::from(vec![
            Span::styled(req.call.name.clone(), Style::default().fg(th().accent3).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(detail, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled(format!("Motivo: {}", req.reason), Style::default().fg(color))),
    ];
    f.render_widget(Paragraph::new(head_lines).wrap(Wrap { trim: false }), head);

    let body_lines: Vec<Line<'static>> = match &req.preview {
        Some(p) if !p.is_empty() => markdown::diff_lines(p, 2000),
        _ => {
            let args = serde_json::to_string_pretty(&req.call.arguments).unwrap_or_default();
            args.lines()
                .map(|l| Line::from(Span::styled(format!("    {l}"), Style::default().fg(th().muted))))
                .collect()
        }
    };
    let body_lines = wrap_all(body_lines, body.width as usize);
    let max = (body_lines.len() as u16).saturating_sub(body.height);
    f.render_widget(Paragraph::new(body_lines).scroll(((*scroll).min(max), 0)), body);

    let foot_lines = if let Some(input) = feedback {
        vec![
            Line::from(Span::styled(
                "¿Qué debería hacer en su lugar? (Enter enviar · Esc volver)",
                Style::default().fg(th().dim),
            )),
            Line::from(vec![
                Span::styled("› ", Style::default().fg(th().accent2)),
                Span::raw(input.text().to_string()),
            ]),
        ]
    } else {
        let mut opts =
            vec![Span::styled(" [s] ", Style::default().fg(th().on_accent).bg(th().ok)), Span::raw(" Sí, una vez   ")];
        if !req.dangerous {
            opts.push(Span::styled(" [a] ", Style::default().fg(th().on_accent).bg(th().accent3)));
            opts.push(Span::raw(format!(" Sí, y no volver a preguntar por {} en esta sesión   ", req.call.name)));
        }
        opts.push(Span::styled(" [n] ", Style::default().fg(th().on_accent).bg(th().err)));
        opts.push(Span::raw(" No   "));
        opts.push(Span::styled(" [d] ", Style::default().fg(th().on_accent).bg(th().muted)));
        opts.push(Span::raw(" No, y decir qué hacer"));
        vec![
            Line::from(""),
            Line::from(opts),
            Line::from(Span::styled("↑↓ PgUp PgDn desplazar la vista previa", Style::default().fg(th().dim))),
        ]
    };
    f.render_widget(Paragraph::new(foot_lines).wrap(Wrap { trim: false }), foot);
}

pub fn draw_sessions(f: &mut Frame, app: &App) {
    let Some(Modal::Sessions { items, selected }) = &app.modal else {
        return;
    };
    let area = centered(f.area(), 80, 70);
    f.render_widget(Clear, area);
    let block = modal_block("Sesiones de este proyecto · Enter reanudar · Esc cerrar", th().accent);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let sel = i == *selected;
            let st = if sel { Style::default().fg(th().on_accent).bg(th().accent) } else { Style::default() };
            let title = if s.title.is_empty() { "(sin título)".to_string() } else { s.title.clone() };
            Line::from(vec![
                Span::styled(if sel { " › " } else { "   " }, st),
                Span::styled(format!("{:<44}", title.chars().take(44).collect::<String>()), st),
                Span::styled(
                    format!("  {}  {:>3} msgs  {}", s.updated_at.format("%Y-%m-%d %H:%M"), s.messages, s.model),
                    if sel { st } else { Style::default().fg(th().dim) },
                ),
            ])
        })
        .collect();
    let scroll = (*selected as u16).saturating_sub(inner.height.saturating_sub(1));
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}
