//! Asistente `/provider add`.

use astrodev_providers::presets::{Preset, PRESETS};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::input::Input;
use crate::theme::*;
use crate::ui::{centered, modal_block};

pub enum Step {
    Preset { selected: usize },
    Form { focus: usize },
    Testing,
    Model { models: Vec<String>, selected: usize, error: Option<String> },
}

pub struct ProviderWizard {
    pub step: Step,
    pub preset: Option<&'static Preset>,
    pub name: Input,
    pub url: Input,
    pub key: Input,
    pub model: Input,
}

/// Lo que la app debe hacer tras una tecla.
pub enum WizardAction {
    None,
    Close,
    /// Guardar la clave (si hay) y probar la conexión.
    Test,
    /// Terminar con el modelo elegido.
    Finish(String),
}

impl ProviderWizard {
    pub fn new() -> Self {
        Self {
            step: Step::Preset { selected: 0 },
            preset: None,
            name: Input::default(),
            url: Input::default(),
            key: Input::default(),
            model: Input::default(),
        }
    }

    fn fields(&self) -> usize {
        if self.preset.is_some_and(|p| p.needs_key) {
            3
        } else {
            2
        }
    }

    fn field_mut(&mut self, i: usize) -> &mut Input {
        match i {
            0 => &mut self.name,
            1 => &mut self.url,
            _ => &mut self.key,
        }
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) -> WizardAction {
        use crossterm::event::KeyCode;
        if k.code == KeyCode::Esc {
            return WizardAction::Close;
        }
        let ctrl = k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
        if ctrl {
            // Ctrl+U vacía el campo actual; el resto de combinaciones se ignoran.
            if k.code == KeyCode::Char('u') {
                match self.step {
                    Step::Form { focus } => self.field_mut(focus).clear(),
                    Step::Model { .. } => self.model.clear(),
                    _ => {}
                }
            }
            return WizardAction::None;
        }
        let fields = self.fields();
        match &mut self.step {
            Step::Preset { selected } => match k.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(PRESETS.len() - 1),
                KeyCode::Enter => {
                    let p = &PRESETS[*selected];
                    self.preset = Some(p);
                    self.name.clear();
                    self.name.insert_str(p.id);
                    self.url.clear();
                    self.url.insert_str(p.base_url);
                    self.step = Step::Form {
                        focus: if p.base_url.is_empty() {
                            1
                        } else if p.needs_key {
                            2
                        } else {
                            1
                        },
                    };
                }
                _ => {}
            },
            Step::Form { focus } => {
                let f = *focus;
                match k.code {
                    KeyCode::Tab | KeyCode::Down => *focus = (f + 1) % fields,
                    KeyCode::BackTab | KeyCode::Up => *focus = (f + fields - 1) % fields,
                    KeyCode::Enter => {
                        if f + 1 < fields {
                            *focus = f + 1;
                        } else if !self.name.text().trim().is_empty() && !self.url.text().trim().is_empty() {
                            self.step = Step::Testing;
                            return WizardAction::Test;
                        }
                    }
                    KeyCode::Backspace => self.field_mut(f).backspace(),
                    KeyCode::Left => self.field_mut(f).left(),
                    KeyCode::Right => self.field_mut(f).right(),
                    KeyCode::Char(c) => self.field_mut(f).insert_char(c),
                    _ => {}
                }
            }
            Step::Testing => {}
            Step::Model { models, selected, .. } => match k.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(models.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let typed = self.model.text().trim().to_string();
                    let m = if !typed.is_empty() { typed } else { models.get(*selected).cloned().unwrap_or_default() };
                    if !m.is_empty() {
                        return WizardAction::Finish(m);
                    }
                }
                KeyCode::Backspace => self.model.backspace(),
                KeyCode::Char(c) => self.model.insert_char(c),
                _ => {}
            },
        }
        WizardAction::None
    }

    pub fn paste(&mut self, s: &str) {
        match self.step {
            Step::Form { focus } => self.field_mut(focus).insert_str(s.trim()),
            Step::Model { .. } => self.model.insert_str(s.trim()),
            _ => {}
        }
    }

    pub fn set_models(&mut self, r: Result<Vec<String>, String>) {
        let default = self.preset.and_then(|p| p.default_model).map(str::to_string);
        match r {
            Ok(models) => {
                let selected = default.as_ref().and_then(|d| models.iter().position(|m| m == d)).unwrap_or(0);
                if models.is_empty() {
                    if let Some(d) = default {
                        self.model.insert_str(&d);
                    }
                }
                self.step = Step::Model { models, selected, error: None };
            }
            Err(e) => {
                if let Some(d) = default {
                    self.model.insert_str(&d);
                }
                self.step = Step::Model { models: Vec::new(), selected: 0, error: Some(e) };
            }
        }
    }
}

fn field_line(label: &str, value: String, focused: bool, placeholder: &str) -> Line<'static> {
    let st = if focused { Style::default().fg(th().text).bg(th().sel_bg) } else { Style::default() };
    let shown = if value.is_empty() {
        Span::styled(placeholder.to_string(), Style::default().fg(th().dim))
    } else {
        Span::styled(value, st)
    };
    Line::from(vec![
        Span::styled(
            format!(" {} {label:<10}", if focused { "›" } else { " " }),
            Style::default().fg(if focused { th().accent2 } else { th().dim }),
        ),
        shown,
    ])
}

pub fn draw(f: &mut Frame, w: &ProviderWizard, spinner: usize) {
    let area = centered(f.area(), 76, 76);
    f.render_widget(Clear, area);
    let block = modal_block("Añadir proveedor · Esc cancelar", th().accent);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [body, help] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);
    let (lines, hint): (Vec<Line>, &str) = match &w.step {
        Step::Preset { selected } => (
            PRESETS
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let sel = i == *selected;
                    let st = if sel { Style::default().fg(th().on_accent).bg(th().accent) } else { Style::default() };
                    Line::from(vec![
                        Span::styled(if sel { " › " } else { "   " }, st),
                        Span::styled(format!("{:<38}", p.label), st.add_modifier(Modifier::BOLD)),
                        Span::styled(
                            if p.local { "  local, sin clave".to_string() } else { format!("  {}", p.base_url) },
                            Style::default().fg(th().dim),
                        ),
                    ])
                })
                .collect(),
            "↑↓ elegir · Enter continuar",
        ),
        Step::Form { focus } => {
            let mut v = vec![
                Line::from(Span::styled(
                    format!(" {}", w.preset.map(|p| p.label).unwrap_or("")),
                    Style::default().fg(th().accent3).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                field_line("Nombre", w.name.text().to_string(), *focus == 0, "nombre en la configuración"),
                field_line("URL base", w.url.text().to_string(), *focus == 1, "https://…/v1"),
            ];
            if w.preset.is_some_and(|p| p.needs_key) {
                let env = w.preset.and_then(|p| p.env_key).unwrap_or("");
                v.push(field_line(
                    "Clave API",
                    "•".repeat(w.key.text().chars().count()),
                    *focus == 2,
                    &format!("pega tu clave (o déjala vacía para usar ${env})"),
                ));
                v.push(Line::from(""));
                v.push(Line::from(Span::styled(
                    "   La clave se guarda en el keyring del sistema, nunca en el archivo de configuración.",
                    Style::default().fg(th().dim),
                )));
            }
            (v, "Tab cambiar campo · Enter siguiente / probar conexión")
        }
        Step::Testing => (
            vec![Line::from(Span::styled(
                format!(" {} Guardando la clave y comprobando la conexión…", SPINNER[spinner % SPINNER.len()]),
                Style::default().fg(th().warn),
            ))],
            "",
        ),
        Step::Model { models, selected, error } => {
            let mut v = Vec::new();
            match error {
                Some(e) => v.push(Line::from(Span::styled(format!(" ✗ {e}"), Style::default().fg(th().err)))),
                None => v.push(Line::from(Span::styled(
                    format!(" ✓ Conexión correcta: {} modelos", models.len()),
                    Style::default().fg(th().ok),
                ))),
            }
            v.push(field_line("Modelo", w.model.text().to_string(), true, "escribe un modelo o elige de la lista"));
            v.push(Line::from(""));
            let height = body.height.saturating_sub(4) as usize;
            let start = selected.saturating_sub(height.saturating_sub(1));
            for (i, m) in models.iter().enumerate().skip(start).take(height) {
                let sel = i == *selected && w.model.is_empty();
                let st = if sel { Style::default().fg(th().on_accent).bg(th().accent) } else { Style::default() };
                v.push(Line::from(Span::styled(format!("   {m}"), st)));
            }
            (v, "↑↓ elegir · escribe para otro modelo · Enter guardar")
        }
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    f.render_widget(Paragraph::new(Span::styled(format!(" {hint}"), Style::default().fg(th().dim))), help);
}
