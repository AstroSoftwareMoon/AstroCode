//! Pantalla `/hardware`: hardware detectado, barras de VRAM/RAM y ranking de modelos.

use astrodev_hardware::{Catalog, Fit, HardwareInfo, Placement, RecommendOptions, Recommendation};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::theme::*;
use crate::ui::{centered, modal_block};

pub const CONTEXTS: &[u32] = &[4096, 8192, 16384, 32768, 65536];

pub struct HardwareView {
    pub hw: Option<HardwareInfo>,
    pub catalog: Option<Catalog>,
    pub recs: Vec<Recommendation>,
    pub selected: usize,
    pub ctx_idx: usize,
    pub error: Option<String>,
}

impl HardwareView {
    pub fn loading() -> Self {
        Self { hw: None, catalog: None, recs: Vec::new(), selected: 0, ctx_idx: 1, error: None }
    }

    pub fn set(&mut self, hw: HardwareInfo, catalog: Catalog) {
        self.hw = Some(hw);
        self.catalog = Some(catalog);
        self.recompute();
    }

    pub fn ctx(&self) -> u32 {
        CONTEXTS[self.ctx_idx]
    }

    pub fn recompute(&mut self) {
        if let (Some(hw), Some(c)) = (&self.hw, &self.catalog) {
            let sel_id = self.recs.get(self.selected).map(|r| r.model_id.clone());
            self.recs =
                astrodev_hardware::recommend(hw, c, &RecommendOptions { ctx: self.ctx(), ..Default::default() });
            self.selected = sel_id.and_then(|id| self.recs.iter().position(|r| r.model_id == id)).unwrap_or(0);
        }
    }

    pub fn selected_rec(&self) -> Option<&Recommendation> {
        self.recs.get(self.selected)
    }
}

fn fit_style(f: Fit) -> (Color, &'static str) {
    match f {
        Fit::Recommended => (Color::Green, "●"),
        Fit::Possible => (Color::Yellow, "◐"),
        Fit::TooBig => (Color::Red, "○"),
    }
}

pub fn draw(f: &mut Frame, view: &HardwareView) {
    let area = centered(f.area(), 96, 92);
    f.render_widget(Clear, area);
    let block =
        modal_block("Hardware y modelos recomendados · ↑↓ elegir · ←→ contexto · Enter instalar con Ollama · l con llama.cpp · Esc cerrar", ACCENT);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(hw) = &view.hw else {
        let msg = view.error.clone().unwrap_or_else(|| "Detectando hardware…".into());
        f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::default().fg(Color::Yellow))), inner);
        return;
    };

    let [summary, table, detail] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(6), Constraint::Length(7)]).areas(inner);

    // --- Resumen de hardware ---
    let mut lines = vec![Line::from(vec![
        Span::styled(" CPU   ", Style::default().fg(DIM)),
        Span::styled(hw.cpu.brand.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(
                "  {} núcleos / {} hilos{}{}",
                hw.cpu.physical_cores,
                hw.cpu.logical_cores,
                if hw.cpu.avx2 { " · AVX2" } else { "" },
                if hw.cpu.avx512 { " · AVX-512" } else { "" }
            ),
            Style::default().fg(Color::Gray),
        ),
    ])];
    let used_ram = hw.ram_total.saturating_sub(hw.ram_available);
    let mut ram = vec![Span::styled(" RAM   ", Style::default().fg(DIM))];
    ram.extend(bar(used_ram as f64 / hw.ram_total.max(1) as f64, 24, CYAN));
    ram.push(Span::raw(format!("  {} en uso de {}", gb(used_ram), gb(hw.ram_total))));
    lines.push(Line::from(ram));
    if hw.gpus.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" GPU   ", Style::default().fg(DIM)),
            Span::styled("ninguna GPU compatible detectada — se usará la CPU", Style::default().fg(Color::Yellow)),
        ]));
    }
    for g in hw.gpus.iter().take(2) {
        let mut s = vec![Span::styled(" GPU   ", Style::default().fg(DIM))];
        let used = g.vram_free.map(|fr| g.vram_total.saturating_sub(fr)).unwrap_or(0);
        s.extend(bar(used as f64 / g.vram_total.max(1) as f64, 24, PINK));
        s.push(Span::raw(format!(
            "  {} · {} {}",
            g.name,
            gb(g.vram_total),
            if g.unified { "(memoria unificada)" } else { "VRAM" }
        )));
        lines.push(Line::from(s));
    }
    lines.push(Line::from(vec![
        Span::styled(" Disco ", Style::default().fg(DIM)),
        Span::raw(format!("{} libres", gb(hw.disk_free))),
        Span::styled("   Backend: ", Style::default().fg(DIM)),
        Span::styled(hw.backend().label(), Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("   Contexto: ", Style::default().fg(DIM)),
        Span::styled(format!("◀ {} ▶", view.ctx()), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(Paragraph::new(lines), summary);

    // --- Tabla de modelos ---
    let header = Row::new(["", "Modelo", "Cuant.", "Descarga", "Memoria", "tok/s", "Estado"])
        .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = view
        .recs
        .iter()
        .map(|r| {
            let (color, icon) = fit_style(r.fit);
            let tps = if r.tokens_per_second > 0.0 { format!("{:.0}", r.tokens_per_second) } else { "—".into() };
            Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color))),
                Cell::from(format!("{}{}", r.model_name, if r.supports_tools { "" } else { " *" })),
                Cell::from(r.quant.clone()),
                Cell::from(gb(r.download_bytes)),
                Cell::from(gb(r.memory.total)),
                Cell::from(tps),
                Cell::from(Span::styled(r.fit.label(), Style::default().fg(color))),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(2),
        Constraint::Min(28),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(17),
    ];
    let t = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(Color::Rgb(0x2A, 0x1F, 0x4A)).add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    let mut st = TableState::default().with_selected(Some(view.selected));
    f.render_stateful_widget(t, table, &mut st);

    // --- Detalle del seleccionado ---
    if let Some(r) = view.selected_rec() {
        draw_detail(f, hw, r, detail);
    }
}

fn draw_detail(f: &mut Frame, hw: &HardwareInfo, r: &Recommendation, area: Rect) {
    let b = astrodev_hardware::recommend::budget(hw);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" {} ", r.model_name),
            Style::default().fg(Color::White).bg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {} · contexto {} · ", r.quant, r.ctx)),
        Span::styled(r.reasons.join(" · "), Style::default().fg(Color::Gray)),
    ])];
    let mem_line = |label: &str, used: u64, cap: u64, color: Color| {
        let mut s = vec![Span::styled(format!(" {label:<6}"), Style::default().fg(DIM))];
        let frac = if cap == 0 { 1.0 } else { used as f64 / cap as f64 };
        s.extend(bar(frac, 30, if frac > 1.0 { Color::Red } else { color }));
        s.push(Span::raw(format!("  {} / {}", gb(used), gb(cap))));
        Line::from(s)
    };
    let m = r.memory;
    match r.placement {
        Placement::FullGpu => lines.push(mem_line(if b.unified { "Unif." } else { "VRAM" }, m.total, b.gpu, PINK)),
        Placement::Partial { .. } => {
            lines.push(mem_line("VRAM", b.gpu, b.gpu, PINK));
            lines.push(mem_line("RAM", m.total.saturating_sub(b.gpu), b.ram, CYAN));
        }
        Placement::Cpu => lines.push(mem_line("RAM", m.total, b.ram, CYAN)),
        Placement::None => lines.push(mem_line("Total", m.total, b.gpu + b.ram, Color::Red)),
    }
    lines.push(Line::from(Span::styled(
        format!(
            " pesos {} + KV cache {} + margen {} = {}{}",
            gb(m.weights),
            gb(m.kv_cache),
            gb(m.overhead),
            gb(m.total),
            r.ollama_pull.as_ref().map(|t| format!("   ·   ollama: {t}")).unwrap_or_default()
        ),
        Style::default().fg(DIM),
    )));
    lines.push(Line::from(Span::styled(
        " * = sin tool calling nativo (AstroDev usa el modo por prompt)",
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), area);
}
