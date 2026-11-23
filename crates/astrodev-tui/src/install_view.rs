//! Pantalla de instalación con pasos, barra de progreso y registro.

use astrodev_installer::InstallEvent;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;
use tokio_util::sync::CancellationToken;

use crate::theme::*;
use crate::ui::{centered, modal_block};

pub struct InstallView {
    pub title: String,
    pub steps: Vec<String>,
    pub log: Vec<String>,
    pub progress: Option<(String, u64, Option<u64>, f64)>,
    /// `Some(Ok(resumen))` o `Some(Err(error))` al terminar.
    pub result: Option<Result<String, String>>,
    pub cancel: CancellationToken,
}

impl InstallView {
    pub fn new(title: impl Into<String>, cancel: CancellationToken) -> Self {
        Self { title: title.into(), steps: Vec::new(), log: Vec::new(), progress: None, result: None, cancel }
    }

    pub fn on_event(&mut self, e: InstallEvent) {
        match e {
            InstallEvent::Step(s) => {
                self.steps.push(s);
                self.progress = None;
            }
            InstallEvent::Progress { label, done, total, bytes_per_sec } => {
                self.progress = Some((label, done, total, bytes_per_sec))
            }
            InstallEvent::Log(l) => {
                self.log.push(l);
                if self.log.len() > 200 {
                    self.log.remove(0);
                }
            }
            InstallEvent::Done(s) => self.result = Some(Ok(s)),
            InstallEvent::Error(e) => self.result = Some(Err(e)),
        }
    }

    pub fn finished(&self) -> bool {
        self.result.is_some()
    }
}

pub fn draw(f: &mut Frame, v: &InstallView, spinner: usize) {
    let area = centered(f.area(), 80, 55);
    f.render_widget(Clear, area);
    let hint = if v.finished() { "Enter/Esc cerrar" } else { "Esc cancelar (las descargas se reanudan)" };
    let block = modal_block(&format!("{} · {hint}", v.title), th().accent);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [steps_a, gauge_a, log_a, result_a] =
        Layout::vertical([Constraint::Length(5), Constraint::Length(3), Constraint::Min(3), Constraint::Length(3)])
            .areas(inner);

    let n = v.steps.len();
    let lines: Vec<Line> = v
        .steps
        .iter()
        .enumerate()
        .skip(n.saturating_sub(5))
        .map(|(i, s)| {
            let current = i + 1 == n && !v.finished();
            let (icon, color) = if current {
                (SPINNER[spinner % SPINNER.len()], th().warn)
            } else if i + 1 == n && matches!(v.result, Some(Err(_))) {
                ("✗", th().err)
            } else {
                ("✓", th().ok)
            };
            Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(color)),
                Span::styled(
                    s.clone(),
                    if current { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() },
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), steps_a);

    if let Some((label, done, total, bps)) = &v.progress {
        let (ratio, text) = match total {
            Some(t) if *t > 0 => (
                (*done as f64 / *t as f64).clamp(0.0, 1.0),
                format!(
                    "{label}  ·  {} / {}  ·  {:.1} MB/s  ·  {:.0} %",
                    gb(*done),
                    gb(*t),
                    bps / 1e6,
                    *done as f64 / *t as f64 * 100.0
                ),
            ),
            _ => (0.0, format!("{label}  ·  {}", gb(*done))),
        };
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(gauge_a);
        f.render_widget(Paragraph::new(Span::styled(format!(" {text}"), Style::default().fg(th().text))), rows[0]);
        let g = Gauge::default().gauge_style(Style::default().fg(th().accent).bg(th().gauge_bg)).ratio(ratio).label("");
        f.render_widget(g, rows[1]);
    }

    let log_lines: Vec<Line> = v
        .log
        .iter()
        .rev()
        .take(log_a.height as usize)
        .rev()
        .map(|l| Line::from(Span::styled(format!("   {l}"), Style::default().fg(th().dim))))
        .collect();
    f.render_widget(Paragraph::new(log_lines), log_a);

    if let Some(r) = &v.result {
        let (text, color) = match r {
            Ok(s) => (format!(" ✓ {s}"), th().ok),
            Err(e) => (format!(" ✗ {e}"), th().err),
        };
        f.render_widget(
            Paragraph::new(Span::styled(text, Style::default().fg(color).add_modifier(Modifier::BOLD)))
                .wrap(Wrap { trim: false }),
            result_a,
        );
    }
}
