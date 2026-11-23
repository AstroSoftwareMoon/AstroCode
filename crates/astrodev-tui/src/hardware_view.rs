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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Recommended,
    Installed,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Ollama,
    Gguf,
}

/// Modelo ya descargado en este equipo.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledModel {
    pub name: String,
    pub size: u64,
    pub source: Source,
    /// Encaje y motivo (del catálogo si se reconoce; si no, estimado por tamaño).
    pub fit: Option<(Fit, String)>,
}

/// Qué debe hacer la app tras una tecla.
pub enum HwAction {
    None,
    Close,
    Install {
        rec: Recommendation,
        ctx: u32,
        llamacpp: bool,
    },
    /// Descargar con Ollama algo que no está en el catálogo.
    InstallTag(String),
    Use(InstalledModel),
    Delete(InstalledModel),
}

pub struct HardwareView {
    pub hw: Option<HardwareInfo>,
    pub catalog: Option<Catalog>,
    pub recs: Vec<Recommendation>,
    pub installed: Vec<InstalledModel>,
    pub installed_loaded: bool,
    pub in_use: String,
    pub tab: Tab,
    pub selected: usize,
    pub ctx_idx: usize,
    pub query: crate::input::Input,
    /// Nombre pendiente de confirmar para borrar (segunda pulsación de `d`).
    pub confirm_delete: Option<String>,
    pub error: Option<String>,
}

impl HardwareView {
    pub fn loading(in_use: String) -> Self {
        Self {
            hw: None,
            catalog: None,
            recs: Vec::new(),
            installed: Vec::new(),
            installed_loaded: false,
            in_use,
            tab: Tab::Recommended,
            selected: 0,
            ctx_idx: 1,
            query: crate::input::Input::default(),
            confirm_delete: None,
            error: None,
        }
    }

    pub fn set(&mut self, hw: HardwareInfo, catalog: Catalog) {
        self.hw = Some(hw);
        self.catalog = Some(catalog);
        self.recompute();
        self.annotate_installed();
    }

    pub fn set_installed(&mut self, models: Vec<InstalledModel>) {
        self.installed = models;
        self.installed_loaded = true;
        self.annotate_installed();
        self.clamp();
    }

    /// Añade a cada instalado si cabe (por catálogo o por tamaño).
    fn annotate_installed(&mut self) {
        let Some(hw) = &self.hw else { return };
        for m in &mut self.installed {
            let by_catalog = self.recs.iter().find(|r| {
                r.ollama_pull.as_deref() == Some(m.name.as_str())
                    || r.gguf_url.as_deref().is_some_and(|u| u.ends_with(&format!("/{}", m.name)))
            });
            m.fit = Some(match by_catalog {
                Some(r) => (r.fit, r.reasons.first().cloned().unwrap_or_default()),
                None => astrodev_hardware::recommend::estimate_by_size(hw, m.size),
            });
        }
    }

    pub fn ctx(&self) -> u32 {
        CONTEXTS[self.ctx_idx]
    }

    pub fn recompute(&mut self) {
        if let (Some(hw), Some(c)) = (&self.hw, &self.catalog) {
            let sel_id = self.selected_rec().map(|r| r.model_id.clone());
            self.recs =
                astrodev_hardware::recommend(hw, c, &RecommendOptions { ctx: self.ctx(), ..Default::default() });
            if self.tab == Tab::Recommended {
                self.selected = sel_id.and_then(|id| self.recs.iter().position(|r| r.model_id == id)).unwrap_or(0);
            }
        }
    }

    /// Resultados de búsqueda sobre todo el catálogo (incluidos los que no caben).
    pub fn search_results(&self) -> Vec<&Recommendation> {
        let q = self.query.text().trim().to_lowercase();
        if q.is_empty() {
            return self.recs.iter().collect();
        }
        let words: Vec<&str> = q.split_whitespace().collect();
        self.recs
            .iter()
            .filter(|r| {
                let hay = format!(
                    "{} {} {}",
                    r.model_name.to_lowercase(),
                    r.model_id,
                    r.ollama_pull.clone().unwrap_or_default().to_lowercase()
                );
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    /// Fila extra "descargar directamente" si la búsqueda parece un nombre de modelo.
    pub fn direct_tag(&self) -> Option<String> {
        let q = self.query.text().trim();
        if q.is_empty() || q.contains(' ') {
            return None;
        }
        let exact = self.recs.iter().any(|r| r.ollama_pull.as_deref() == Some(q) || r.model_id == q);
        (!exact).then(|| q.to_string())
    }

    fn rows(&self) -> usize {
        match self.tab {
            Tab::Recommended => self.recs.len(),
            Tab::Installed => self.installed.len(),
            Tab::Search => self.search_results().len() + usize::from(self.direct_tag().is_some()),
        }
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.rows().saturating_sub(1));
    }

    pub fn selected_rec(&self) -> Option<&Recommendation> {
        match self.tab {
            Tab::Recommended => self.recs.get(self.selected),
            Tab::Search => self.search_results().get(self.selected).copied(),
            Tab::Installed => None,
        }
    }

    fn switch(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
        self.confirm_delete = None;
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) -> HwAction {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let typing = self.tab == Tab::Search;
        match k.code {
            KeyCode::Esc => {
                if typing && !self.query.is_empty() {
                    self.query.clear();
                    self.selected = 0;
                    return HwAction::None;
                }
                return HwAction::Close;
            }
            KeyCode::Tab => {
                let next = match self.tab {
                    Tab::Recommended => Tab::Installed,
                    Tab::Installed => Tab::Search,
                    Tab::Search => Tab::Recommended,
                };
                self.switch(next);
            }
            KeyCode::BackTab => {
                let prev = match self.tab {
                    Tab::Recommended => Tab::Search,
                    Tab::Installed => Tab::Recommended,
                    Tab::Search => Tab::Installed,
                };
                self.switch(prev);
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(self.rows().saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(self.rows().saturating_sub(1)),
            KeyCode::Left if !typing || self.query.is_empty() => {
                self.ctx_idx = self.ctx_idx.saturating_sub(1);
                self.recompute();
            }
            KeyCode::Right if !typing || self.query.is_empty() => {
                self.ctx_idx = (self.ctx_idx + 1).min(CONTEXTS.len() - 1);
                self.recompute();
            }
            KeyCode::Enter => return self.activate(false),
            KeyCode::Char('u') if ctrl && typing => {
                self.query.clear();
                self.selected = 0;
            }
            _ if typing => match k.code {
                KeyCode::Backspace => {
                    self.query.backspace();
                    self.selected = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    self.query.insert_char(c);
                    self.selected = 0;
                }
                _ => {}
            },
            KeyCode::Char('/') | KeyCode::Char('b') => self.switch(Tab::Search),
            KeyCode::Char('i') => self.switch(Tab::Installed),
            KeyCode::Char('r') => self.switch(Tab::Recommended),
            KeyCode::Char('q') => return HwAction::Close,
            KeyCode::Char('l') if self.tab == Tab::Recommended => return self.activate(true),
            KeyCode::Char('d') | KeyCode::Delete if self.tab == Tab::Installed => {
                if let Some(m) = self.installed.get(self.selected).cloned() {
                    if self.confirm_delete.as_deref() == Some(m.name.as_str()) {
                        self.confirm_delete = None;
                        return HwAction::Delete(m);
                    }
                    self.confirm_delete = Some(m.name);
                }
            }
            _ => {}
        }
        HwAction::None
    }

    fn activate(&mut self, llamacpp: bool) -> HwAction {
        match self.tab {
            Tab::Installed => self.installed.get(self.selected).cloned().map(HwAction::Use).unwrap_or(HwAction::None),
            Tab::Recommended | Tab::Search => {
                let results = self.search_results().len();
                if self.tab == Tab::Search && self.selected == results {
                    if let Some(t) = self.direct_tag() {
                        return HwAction::InstallTag(t);
                    }
                }
                match self.selected_rec().cloned() {
                    Some(rec) => HwAction::Install { rec, ctx: self.ctx(), llamacpp },
                    None => HwAction::None,
                }
            }
        }
    }
}

fn fit_style(f: Fit) -> (Color, &'static str) {
    match f {
        Fit::Recommended => (th().ok, "●"),
        Fit::Possible => (th().warn, "◐"),
        Fit::TooBig => (th().err, "○"),
    }
}

pub fn draw(f: &mut Frame, view: &HardwareView) {
    let area = centered(f.area(), 96, 92);
    f.render_widget(Clear, area);
    let block = modal_block("Modelos · Tab cambiar pestaña · Esc cerrar", th().accent);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(hw) = &view.hw else {
        let msg = view.error.clone().unwrap_or_else(|| "Detectando hardware…".into());
        f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::default().fg(th().warn))), inner);
        return;
    };

    let [summary, tabs, table, detail] =
        Layout::vertical([Constraint::Length(5), Constraint::Length(2), Constraint::Min(5), Constraint::Length(7)])
            .areas(inner);

    // --- Resumen de hardware ---
    let mut lines = vec![Line::from(vec![
        Span::styled(" CPU   ", Style::default().fg(th().dim)),
        Span::styled(hw.cpu.brand.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(
                "  {} núcleos / {} hilos{}{}",
                hw.cpu.physical_cores,
                hw.cpu.logical_cores,
                if hw.cpu.avx2 { " · AVX2" } else { "" },
                if hw.cpu.avx512 { " · AVX-512" } else { "" }
            ),
            Style::default().fg(th().muted),
        ),
    ])];
    let used_ram = hw.ram_total.saturating_sub(hw.ram_available);
    let mut ram = vec![Span::styled(" RAM   ", Style::default().fg(th().dim))];
    ram.extend(bar(used_ram as f64 / hw.ram_total.max(1) as f64, 24, th().accent3));
    ram.push(Span::raw(format!("  {} en uso de {}", gb(used_ram), gb(hw.ram_total))));
    lines.push(Line::from(ram));
    if hw.gpus.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" GPU   ", Style::default().fg(th().dim)),
            Span::styled("ninguna GPU compatible detectada — se usará la CPU", Style::default().fg(th().warn)),
        ]));
    }
    for g in hw.gpus.iter().take(2) {
        let mut s = vec![Span::styled(" GPU   ", Style::default().fg(th().dim))];
        let used = g.vram_free.map(|fr| g.vram_total.saturating_sub(fr)).unwrap_or(0);
        s.extend(bar(used as f64 / g.vram_total.max(1) as f64, 24, th().accent2));
        s.push(Span::raw(format!(
            "  {} · {} {}",
            g.name,
            gb(g.vram_total),
            if g.unified { "(memoria unificada)" } else { "VRAM" }
        )));
        lines.push(Line::from(s));
    }
    lines.push(Line::from(vec![
        Span::styled(" Disco ", Style::default().fg(th().dim)),
        Span::raw(format!("{} libres", gb(hw.disk_free))),
        Span::styled("   Backend: ", Style::default().fg(th().dim)),
        Span::styled(hw.backend().label(), Style::default().fg(th().accent3).add_modifier(Modifier::BOLD)),
        Span::styled("   Contexto: ", Style::default().fg(th().dim)),
        Span::styled(format!("◀ {} ▶", view.ctx()), Style::default().fg(th().text).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(Paragraph::new(lines), summary);

    // --- Pestañas ---
    let tab_span = |label: String, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default().fg(th().on_accent).bg(th().accent).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(th().dim))
        }
    };
    let installed_label = if view.installed_loaded {
        format!("Instalados ({})", view.installed.len())
    } else {
        "Instalados (…)".into()
    };
    let mut tl = vec![
        Span::raw(" "),
        tab_span(
            format!("Recomendados ({})", view.recs.iter().filter(|r| r.fit != Fit::TooBig).count()),
            view.tab == Tab::Recommended,
        ),
        Span::raw(" "),
        tab_span(installed_label, view.tab == Tab::Installed),
        Span::raw(" "),
        tab_span("Buscar".into(), view.tab == Tab::Search),
    ];
    if view.tab == Tab::Search {
        tl.push(Span::styled("  🔍 ", Style::default().fg(th().dim)));
        if view.query.is_empty() {
            tl.push(Span::styled("escribe: qwen, coder, 14b, deepseek-r1:8b, hf.co/…", Style::default().fg(th().dim)));
        } else {
            tl.push(Span::styled(view.query.text().to_string(), Style::default().add_modifier(Modifier::BOLD)));
            tl.push(Span::styled("▏", Style::default().fg(th().accent)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(tl)), tabs);

    match view.tab {
        Tab::Installed => draw_installed(f, view, table, detail),
        Tab::Recommended | Tab::Search => {
            let list: Vec<&Recommendation> =
                if view.tab == Tab::Search { view.search_results() } else { view.recs.iter().collect() };
            draw_recs(f, view, &list, table);
            let direct_selected = view.tab == Tab::Search && view.selected == list.len();
            if direct_selected {
                let t = view.direct_tag().unwrap_or_default();
                let lines = vec![
                    Line::from(vec![
                        Span::styled(format!(" {t} "), Style::default().fg(th().on_accent).bg(th().accent).add_modifier(Modifier::BOLD)),
                        Span::raw("  no está en el catálogo"),
                    ]),
                    Line::from(Span::styled(
                        " Enter lo descarga con Ollama: vale una etiqueta de ollama.com/library (deepseek-r1:8b) o un GGUF de Hugging Face (hf.co/usuario/repo:Q4_K_M).",
                        Style::default().fg(th().dim),
                    )),
                    Line::from(Span::styled(" Tras descargarlo se mide su velocidad y se ajusta el contexto.", Style::default().fg(th().dim))),
                ];
                f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), detail);
            } else if let Some(r) = view.selected_rec() {
                draw_detail(f, hw, r, detail);
            } else if view.tab == Tab::Search {
                f.render_widget(
                    Paragraph::new(Span::styled(" Sin resultados en el catálogo.", Style::default().fg(th().dim))),
                    detail,
                );
            }
        }
    }
}

fn draw_recs(f: &mut Frame, view: &HardwareView, list: &[&Recommendation], area: Rect) {
    let header = Row::new(["", "Modelo", "Cuant.", "Descarga", "Memoria", "tok/s", "Estado"])
        .style(Style::default().fg(th().dim).add_modifier(Modifier::BOLD));
    let mut rows: Vec<Row> = list
        .iter()
        .map(|r| {
            let (color, icon) = fit_style(r.fit);
            let tps = if r.tokens_per_second > 0.0 { format!("{:.0}", r.tokens_per_second) } else { "—".into() };
            let installed = view.installed.iter().any(|m| r.ollama_pull.as_deref() == Some(m.name.as_str()));
            Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color))),
                Cell::from(format!(
                    "{}{}{}",
                    r.model_name,
                    if r.supports_tools { "" } else { " *" },
                    if installed { "  ✓ instalado" } else { "" }
                )),
                Cell::from(r.quant.clone()),
                Cell::from(gb(r.download_bytes)),
                Cell::from(gb(r.memory.total)),
                Cell::from(tps),
                Cell::from(Span::styled(r.fit.label(), Style::default().fg(color))),
            ])
        })
        .collect();
    if view.tab == Tab::Search {
        if let Some(t) = view.direct_tag() {
            rows.push(Row::new(vec![
                Cell::from(Span::styled("↓", Style::default().fg(th().accent))),
                Cell::from(Span::styled(
                    format!("Descargar «{t}» con Ollama"),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(Span::styled("externo", Style::default().fg(th().dim))),
            ]));
        }
    }
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
        .row_highlight_style(Style::default().bg(th().sel_bg).add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    let mut st = TableState::default().with_selected(Some(view.selected));
    f.render_stateful_widget(t, area, &mut st);
}

fn draw_installed(f: &mut Frame, view: &HardwareView, area: Rect, detail: Rect) {
    if !view.installed_loaded {
        f.render_widget(
            Paragraph::new(Span::styled(" Buscando modelos instalados…", Style::default().fg(th().dim))),
            area,
        );
        return;
    }
    if view.installed.is_empty() {
        let lines = vec![
            Line::from(" No hay modelos instalados todavía."),
            Line::from(Span::styled(
                " Pestaña Recomendados para instalar uno que te quepa, o Buscar para cualquier otro.",
                Style::default().fg(th().dim),
            )),
        ];
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let header = Row::new(["", "Modelo", "Runtime", "Tamaño", "En tu equipo"])
        .style(Style::default().fg(th().dim).add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = view
        .installed
        .iter()
        .map(|m| {
            let (fit, why) = m.fit.clone().unwrap_or((Fit::Possible, String::new()));
            let (color, icon) = fit_style(fit);
            let in_use = m.name == view.in_use;
            Row::new(vec![
                Cell::from(Span::styled(icon, Style::default().fg(color))),
                Cell::from(Span::styled(
                    format!("{}{}", m.name, if in_use { "  ● en uso" } else { "" }),
                    if in_use { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() },
                )),
                Cell::from(match m.source {
                    Source::Ollama => "Ollama",
                    Source::Gguf => "llama.cpp",
                }),
                Cell::from(gb(m.size)),
                Cell::from(Span::styled(why, Style::default().fg(color))),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(2),
        Constraint::Min(30),
        Constraint::Length(10),
        Constraint::Length(9),
        Constraint::Min(24),
    ];
    let t = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(th().sel_bg).add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    let mut st = TableState::default().with_selected(Some(view.selected));
    f.render_stateful_widget(t, area, &mut st);

    let mut lines = vec![Line::from("")];
    match &view.confirm_delete {
        Some(name) => lines.push(Line::from(Span::styled(
            format!(" ¿Borrar {name}? Pulsa d otra vez para confirmar (libera su espacio en disco)."),
            Style::default().fg(th().err).add_modifier(Modifier::BOLD),
        ))),
        None => lines.push(Line::from(Span::styled(
            " Enter usar este modelo · d borrar · / buscar otro · ↑↓ elegir",
            Style::default().fg(th().dim),
        ))),
    }
    let total: u64 = view.installed.iter().map(|m| m.size).sum();
    lines.push(Line::from(Span::styled(
        format!(" {} modelos · {} en disco", view.installed.len(), gb(total)),
        Style::default().fg(th().dim),
    )));
    f.render_widget(Paragraph::new(lines), detail);
}

fn draw_detail(f: &mut Frame, hw: &HardwareInfo, r: &Recommendation, area: Rect) {
    let b = astrodev_hardware::recommend::budget(hw);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" {} ", r.model_name),
            Style::default().fg(th().on_accent).bg(th().accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {} · contexto {} · ", r.quant, r.ctx)),
        Span::styled(r.reasons.join(" · "), Style::default().fg(th().muted)),
    ])];
    let mem_line = |label: &str, used: u64, cap: u64, color: Color| {
        let mut s = vec![Span::styled(format!(" {label:<6}"), Style::default().fg(th().dim))];
        let frac = if cap == 0 { 1.0 } else { used as f64 / cap as f64 };
        s.extend(bar(frac, 30, if frac > 1.0 { th().err } else { color }));
        s.push(Span::raw(format!("  {} / {}", gb(used), gb(cap))));
        Line::from(s)
    };
    let m = r.memory;
    match r.placement {
        Placement::FullGpu => {
            lines.push(mem_line(if b.unified { "Unif." } else { "VRAM" }, m.total, b.gpu, th().accent2))
        }
        Placement::Partial { .. } => {
            lines.push(mem_line("VRAM", b.gpu, b.gpu, th().accent2));
            lines.push(mem_line("RAM", m.total.saturating_sub(b.gpu), b.ram, th().accent3));
        }
        Placement::Cpu => lines.push(mem_line("RAM", m.total, b.ram, th().accent3)),
        Placement::None => lines.push(mem_line("Total", m.total, b.gpu + b.ram, th().err)),
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
        Style::default().fg(th().dim),
    )));
    lines.push(Line::from(Span::styled(
        " Enter instalar con Ollama · l con llama.cpp · ←→ contexto · / buscar · * sin tool calling nativo",
        Style::default().fg(th().dim),
    )));
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), area);
}
