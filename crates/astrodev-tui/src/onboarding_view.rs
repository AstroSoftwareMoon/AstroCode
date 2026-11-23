//! Asistente de primera ejecución: hardware → local/API → recomendar e instalar → proyecto.

use astrodev_hardware::{Catalog, Fit, HardwareInfo, RecommendOptions, Recommendation};
use astrodev_installer::runtime::RuntimeStatus;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::theme::*;
use crate::ui::{centered, modal_block};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Api,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Hardware,
    Mode,
    Local,
    Project,
}

pub struct Onboarding {
    pub step: Step,
    pub hw: Option<HardwareInfo>,
    pub recs: Vec<Recommendation>,
    pub ollama: Option<RuntimeStatus>,
    pub ollama_models: Vec<String>,
    pub mode_sel: usize,
    pub model_sel: usize,
    pub mode: Option<Mode>,
    pub project: String,
    pub has_astro_md: bool,
    /// Resumen de lo configurado (para el último paso).
    pub done: Vec<String>,
}

pub enum OnboardAction {
    None,
    /// Cerrar el asistente (Esc).
    Skip,
    InstallModel(Recommendation),
    UseInstalledModel(String),
    AddProvider,
    Finish {
        init_astro_md: bool,
    },
}

impl Onboarding {
    pub fn new(project: String, has_astro_md: bool) -> Self {
        Self {
            step: Step::Hardware,
            hw: None,
            recs: Vec::new(),
            ollama: None,
            ollama_models: Vec::new(),
            mode_sel: 0,
            model_sel: 0,
            mode: None,
            project,
            has_astro_md,
            done: Vec::new(),
        }
    }

    pub fn set_hardware(&mut self, hw: HardwareInfo, catalog: &Catalog, ollama: RuntimeStatus, models: Vec<String>) {
        self.recs = astrodev_hardware::recommend(&hw, catalog, &RecommendOptions::default())
            .into_iter()
            .filter(|r| r.fit != Fit::TooBig)
            .take(8)
            .collect();
        // Sin modelos locales viables, sugerir API.
        self.mode_sel = if self.recs.iter().any(|r| r.fit == Fit::Recommended) { 0 } else { 1 };
        self.hw = Some(hw);
        self.ollama = Some(ollama);
        self.ollama_models = models;
    }

    fn local_items(&self) -> usize {
        self.recs.len() + usize::from(!self.ollama_models.is_empty())
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) -> OnboardAction {
        use crossterm::event::KeyCode;
        if k.code == KeyCode::Esc {
            return OnboardAction::Skip;
        }
        match self.step {
            Step::Hardware => {
                if k.code == KeyCode::Enter && self.hw.is_some() {
                    self.step = Step::Mode;
                }
            }
            Step::Mode => match k.code {
                KeyCode::Up => self.mode_sel = self.mode_sel.saturating_sub(1),
                KeyCode::Down => self.mode_sel = (self.mode_sel + 1).min(2),
                KeyCode::Left | KeyCode::Backspace => self.step = Step::Hardware,
                KeyCode::Enter => {
                    let m = [Mode::Local, Mode::Api, Mode::Both][self.mode_sel];
                    self.mode = Some(m);
                    if m == Mode::Api {
                        return OnboardAction::AddProvider;
                    }
                    self.step = Step::Local;
                }
                _ => {}
            },
            Step::Local => match k.code {
                KeyCode::Up => self.model_sel = self.model_sel.saturating_sub(1),
                KeyCode::Down => self.model_sel = (self.model_sel + 1).min(self.local_items().saturating_sub(1)),
                KeyCode::Left | KeyCode::Backspace => self.step = Step::Mode,
                KeyCode::Char('s') => return self.after_local(),
                KeyCode::Enter => {
                    let offset = usize::from(!self.ollama_models.is_empty());
                    if offset == 1 && self.model_sel == 0 {
                        let pick = self
                            .ollama_models
                            .iter()
                            .find(|m| m.contains("code"))
                            .unwrap_or(&self.ollama_models[0])
                            .clone();
                        return OnboardAction::UseInstalledModel(pick);
                    }
                    if let Some(r) = self.recs.get(self.model_sel - offset) {
                        return OnboardAction::InstallModel(r.clone());
                    }
                }
                _ => {}
            },
            Step::Project => match k.code {
                KeyCode::Enter => return OnboardAction::Finish { init_astro_md: false },
                KeyCode::Char('i') if !self.has_astro_md => return OnboardAction::Finish { init_astro_md: true },
                _ => {}
            },
        }
        OnboardAction::None
    }

    /// Tras el paso local: si eligió "ambos", toca la API; si no, el proyecto.
    pub fn after_local(&mut self) -> OnboardAction {
        if self.mode == Some(Mode::Both) {
            self.mode = Some(Mode::Api);
            return OnboardAction::AddProvider;
        }
        self.step = Step::Project;
        OnboardAction::None
    }
}

fn title_for(step: Step) -> (usize, &'static str) {
    match step {
        Step::Hardware => (1, "Tu equipo"),
        Step::Mode => (2, "Local, API o ambos"),
        Step::Local => (3, "Elegir e instalar un modelo"),
        Step::Project => (4, "Abrir el proyecto"),
    }
}

pub fn draw(f: &mut Frame, o: &Onboarding, spinner: usize) {
    let area = centered(f.area(), 84, 86);
    f.render_widget(Clear, area);
    let (n, title) = title_for(o.step);
    let block = modal_block(&format!("Bienvenido a AstroDev · Paso {n} de 4: {title}"), th().accent);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [dots, body, help] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(3), Constraint::Length(1)]).areas(inner);

    let mut d = vec![Span::raw(" ")];
    for i in 1..=4 {
        d.push(Span::styled(
            if i <= n { "● " } else { "○ " },
            Style::default().fg(if i <= n { th().accent2 } else { th().dim }),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(d)), dots);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(th().dim);
    let (lines, hint): (Vec<Line<'static>>, String) = match o.step {
        Step::Hardware => match &o.hw {
            None => (
                vec![Line::from(Span::styled(
                    format!(" {} Detectando tu hardware…", SPINNER[spinner % SPINNER.len()]),
                    Style::default().fg(th().warn),
                ))],
                String::new(),
            ),
            Some(hw) => {
                let mut v = vec![
                    Line::from(vec![
                        Span::styled(" CPU    ", dim),
                        Span::styled(hw.cpu.brand.clone(), bold),
                        Span::raw(format!(" · {} núcleos", hw.cpu.physical_cores)),
                    ]),
                    Line::from(vec![Span::styled(" RAM    ", dim), Span::raw(gb(hw.ram_total))]),
                ];
                if hw.gpus.is_empty() {
                    v.push(Line::from(vec![
                        Span::styled(" GPU    ", dim),
                        Span::styled("sin GPU compatible: los modelos irán en CPU", Style::default().fg(th().warn)),
                    ]));
                }
                for g in &hw.gpus {
                    v.push(Line::from(vec![
                        Span::styled(" GPU    ", dim),
                        Span::styled(g.name.clone(), bold),
                        Span::raw(format!(" · {} {}", gb(g.vram_total), if g.unified { "unificada" } else { "VRAM" })),
                    ]));
                }
                v.push(Line::from(vec![
                    Span::styled(" Disco  ", dim),
                    Span::raw(format!("{} libres", gb(hw.disk_free))),
                ]));
                v.push(Line::from(""));
                let verdict = match o.recs.iter().find(|r| r.fit == Fit::Recommended) {
                    Some(best) => format!(
                        " ✓ Puedes usar modelos locales. El mejor para tu equipo: {} (~{:.0} tok/s).",
                        best.model_name, best.tokens_per_second
                    ),
                    None => {
                        " Tu equipo va justo para modelos locales: te recomendamos usar una API (o un modelo pequeño)."
                            .into()
                    }
                };
                v.push(Line::from(Span::styled(verdict, Style::default().fg(th().ok).add_modifier(Modifier::BOLD))));
                (v, "Enter continuar · Esc saltar el asistente".into())
            }
        },
        Step::Mode => {
            let opts = [
                (
                    "Local",
                    "Modelos en tu equipo: privado, gratis y sin conexión. AstroDev instala Ollama si hace falta.",
                ),
                ("API", "Claude, GPT, Gemini, OpenRouter, Groq, DeepSeek, Mistral… Más potentes; necesitas una clave."),
                ("Ambos", "Un modelo local para el día a día y una API para lo difícil (o como fallback)."),
            ];
            let mut v = Vec::new();
            for (i, (name, desc)) in opts.iter().enumerate() {
                let sel = i == o.mode_sel;
                let st = if sel {
                    Style::default().fg(th().on_accent).bg(th().accent).add_modifier(Modifier::BOLD)
                } else {
                    bold
                };
                v.push(Line::from(vec![
                    Span::styled(if sel { " › " } else { "   " }, st),
                    Span::styled(format!("{name:<8}"), st),
                ]));
                v.push(Line::from(Span::styled(format!("     {desc}"), dim)));
                v.push(Line::from(""));
            }
            (v, "↑↓ elegir · Enter continuar · ← atrás".into())
        }
        Step::Local => {
            let mut v = Vec::new();
            let status = match &o.ollama {
                Some(s) if s.running => format!("Ollama {} en marcha", s.version.clone().unwrap_or_default()),
                Some(s) if s.binary.is_some() => "Ollama instalado (lo arrancaré)".into(),
                _ => "Ollama no está instalado: lo instalaré automáticamente (sin sudo)".into(),
            };
            v.push(Line::from(vec![
                Span::styled(" Runtime: ", dim),
                Span::styled(status, Style::default().fg(th().accent3)),
            ]));
            v.push(Line::from(""));
            let mut idx = 0;
            if !o.ollama_models.is_empty() {
                let sel = o.model_sel == 0;
                let st = if sel { Style::default().fg(th().on_accent).bg(th().accent) } else { Style::default() };
                v.push(Line::from(Span::styled(
                    format!(
                        " {} Usar un modelo que ya tienes: {}",
                        if sel { "›" } else { " " },
                        o.ollama_models.join(", ")
                    ),
                    st,
                )));
                v.push(Line::from(""));
                idx = 1;
            }
            for (i, r) in o.recs.iter().enumerate() {
                let sel = o.model_sel == i + idx;
                let st = if sel { Style::default().fg(th().on_accent).bg(th().accent) } else { Style::default() };
                let (c, icon) = if r.fit == Fit::Recommended { (th().ok, "●") } else { (th().warn, "◐") };
                v.push(Line::from(vec![
                    Span::styled(format!(" {} ", if sel { "›" } else { " " }), st),
                    Span::styled(format!("{icon} "), Style::default().fg(c)),
                    Span::styled(format!("{:<34}", r.model_name), st.add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(" {:<7} {:>8}  ~{:>3.0} tok/s", r.quant, gb(r.download_bytes), r.tokens_per_second),
                        st,
                    ),
                ]));
            }
            (v, "↑↓ elegir · Enter instalar · s saltar · ← atrás".into())
        }
        Step::Project => {
            let mut v = vec![
                Line::from(vec![Span::styled(" Proyecto: ", dim), Span::styled(o.project.clone(), bold)]),
                Line::from(""),
            ];
            for d in &o.done {
                v.push(Line::from(Span::styled(format!(" ✓ {d}"), Style::default().fg(th().ok))));
            }
            v.push(Line::from(""));
            if o.has_astro_md {
                v.push(Line::from(Span::styled(
                    " ✓ ASTRO.md encontrado: lo usaré como contexto del proyecto.",
                    Style::default().fg(th().ok),
                )));
            } else {
                v.push(Line::from(" Este proyecto no tiene ASTRO.md (instrucciones para el agente)."));
                v.push(Line::from(Span::styled(" Pulsa i para que AstroDev lo analice y lo cree.", dim)));
            }
            v.push(Line::from(""));
            v.push(Line::from(Span::styled(
                " Consejos: escribe lo que quieres hacer · /help comandos · Shift+Tab modo de permisos",
                dim,
            )));
            (
                v,
                if o.has_astro_md {
                    "Enter empezar".into()
                } else {
                    "Enter empezar · i crear ASTRO.md y empezar".into()
                },
            )
        }
    };
    f.render_widget(Paragraph::new(crate::wrap::wrap_all(lines, body.width as usize)), body);
    f.render_widget(Paragraph::new(Span::styled(format!(" {hint}"), dim)), help);
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_hardware::detect::{parse_nvidia_smi, CpuInfo};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ready() -> Onboarding {
        let mut o = Onboarding::new("/p".into(), false);
        let hw = HardwareInfo {
            os: "linux".into(),
            arch: "x86_64".into(),
            cpu: CpuInfo {
                brand: "x".into(),
                physical_cores: 8,
                logical_cores: 16,
                avx2: true,
                avx512: false,
                neon: false,
            },
            ram_total: 32_000_000_000,
            ram_available: 30_000_000_000,
            gpus: parse_nvidia_smi("RTX 4070, 12282, 12000, 1"),
            disk_free: 200_000_000_000,
        };
        let st = RuntimeStatus {
            kind: astrodev_installer::runtime::RuntimeKind::Ollama,
            binary: None,
            managed: false,
            running: false,
            version: None,
        };
        o.set_hardware(hw, &Catalog::embedded(), st, vec![]);
        o
    }

    #[test]
    fn local_flow_reaches_install() {
        let mut o = ready();
        assert!(!o.recs.is_empty());
        assert!(matches!(o.on_key(key(KeyCode::Enter)), OnboardAction::None));
        assert_eq!(o.step, Step::Mode);
        assert_eq!(o.mode_sel, 0, "con GPU se sugiere local");
        o.on_key(key(KeyCode::Enter));
        assert_eq!(o.step, Step::Local);
        assert!(matches!(o.on_key(key(KeyCode::Enter)), OnboardAction::InstallModel(_)));
    }

    #[test]
    fn api_and_both_flows() {
        let mut o = ready();
        o.on_key(key(KeyCode::Enter));
        o.on_key(key(KeyCode::Down));
        assert!(matches!(o.on_key(key(KeyCode::Enter)), OnboardAction::AddProvider));

        let mut o = ready();
        o.on_key(key(KeyCode::Enter));
        o.on_key(key(KeyCode::Down));
        o.on_key(key(KeyCode::Down));
        o.on_key(key(KeyCode::Enter));
        assert_eq!(o.step, Step::Local);
        // tras el local (saltado con s) toca la API
        assert!(matches!(o.on_key(key(KeyCode::Char('s'))), OnboardAction::AddProvider));
        assert!(matches!(o.after_local(), OnboardAction::None));
        assert_eq!(o.step, Step::Project);
        assert!(matches!(o.on_key(key(KeyCode::Char('i'))), OnboardAction::Finish { init_astro_md: true }));
    }
}
