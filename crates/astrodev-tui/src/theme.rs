//! Paleta de colores. Dos temas:
//! - `mono` (por defecto): minimalista en blanco y negro, sólo grises.
//! - `astro`: los colores de la marca (magenta → violeta → cian).
//!
//! Se elige con `[ui] theme` en la configuración o `/theme`; `NO_COLOR` fuerza `mono`.

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub struct Palette {
    pub name: &'static str,
    /// Color principal (marcos, cabecera, selección).
    pub accent: Color,
    /// Secundario (prompt del usuario).
    pub accent2: Color,
    /// Terciario (nombres de herramientas, modelo).
    pub accent3: Color,
    pub ok: Color,
    pub err: Color,
    pub warn: Color,
    pub info: Color,
    pub dim: Color,
    pub muted: Color,
    pub code: Color,
    pub sel_bg: Color,
    /// Texto sobre fondo `accent`.
    pub on_accent: Color,
    /// Texto destacado sobre `sel_bg`.
    pub text: Color,
    pub gauge_bg: Color,
    pub diff_add: Color,
    pub diff_del: Color,
    /// Logo con degradado de colores.
    pub gradient: bool,
}

pub const MONO: Palette = Palette {
    name: "mono",
    accent: Color::White,
    accent2: Color::White,
    accent3: Color::Gray,
    ok: Color::White,
    err: Color::White,
    warn: Color::Gray,
    info: Color::Gray,
    dim: Color::DarkGray,
    muted: Color::Gray,
    code: Color::Gray,
    sel_bg: Color::Rgb(0x2E, 0x2E, 0x2E),
    on_accent: Color::Black,
    text: Color::White,
    gauge_bg: Color::Rgb(0x26, 0x26, 0x26),
    diff_add: Color::White,
    diff_del: Color::DarkGray,
    gradient: false,
};

pub const ASTRO: Palette = Palette {
    name: "astro",
    accent: Color::Rgb(0x9B, 0x5C, 0xFF),
    accent2: Color::Rgb(0xFF, 0x4F, 0xD8),
    accent3: Color::Rgb(0x3E, 0xC8, 0xFF),
    ok: Color::Green,
    err: Color::Red,
    warn: Color::Yellow,
    info: Color::Blue,
    dim: Color::DarkGray,
    muted: Color::Gray,
    code: Color::LightYellow,
    sel_bg: Color::Rgb(0x2A, 0x1F, 0x4A),
    on_accent: Color::White,
    text: Color::White,
    gauge_bg: Color::Rgb(0x22, 0x1A, 0x38),
    diff_add: Color::Green,
    diff_del: Color::Red,
    gradient: true,
};

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Paleta activa.
pub fn th() -> &'static Palette {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        &ASTRO
    } else {
        &MONO
    }
}

/// Cambia el tema. Devuelve `false` si el nombre no existe.
pub fn set_theme(name: &str) -> bool {
    let id = match name.trim().to_lowercase().as_str() {
        "mono" | "minimal" | "bn" | "blanco-y-negro" => 0,
        "astro" | "color" | "colores" => 1,
        _ => return false,
    };
    let forced_mono = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    CURRENT.store(if forced_mono { 0 } else { id }, Ordering::Relaxed);
    true
}

pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub const LOGO: &[&str] = &[
    r"    _        _             ____             ",
    r"   / \   ___| |_ _ __ ___ |  _ \  _____   __",
    r"  / _ \ / __| __| '__/ _ \| | | |/ _ \ \ / /",
    r" / ___ \\__ \ |_| | | (_) | |_| |  __/\ V / ",
    r"/_/   \_\___/\__|_|  \___/|____/ \___| \_/  ",
];

/// Colores de la marca: magenta → violeta → cian (igual que el logo a color).
const BRAND: [(u8, u8, u8); 3] = [(0xFF, 0x4F, 0xD8), (0x9B, 0x5C, 0xFF), (0x3E, 0xC8, 0xFF)];

pub fn brand_color(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b, u) = if t < 0.55 { (BRAND[0], BRAND[1], t / 0.55) } else { (BRAND[1], BRAND[2], (t - 0.55) / 0.45) };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Línea del logo: degradado en `astro`, blanco en `mono`.
pub fn gradient_line(s: &str) -> Line<'static> {
    if !th().gradient {
        return Line::from(Span::styled(s.to_string(), Style::default().fg(th().text).add_modifier(Modifier::BOLD)));
    }
    let n = s.chars().count().max(1) as f32;
    Line::from(
        s.chars()
            .enumerate()
            .map(|(i, c)| {
                Span::styled(c.to_string(), Style::default().fg(brand_color(i as f32 / n)).add_modifier(Modifier::BOLD))
            })
            .collect::<Vec<_>>(),
    )
}

/// Barra de progreso de texto: `██████░░░░`.
pub fn bar(frac: f64, width: usize, color: Color) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let full = (frac * width as f64).round() as usize;
    vec![
        Span::styled("█".repeat(full), Style::default().fg(color)),
        Span::styled("░".repeat(width - full), Style::default().fg(th().dim)),
    ]
}

pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_themes() {
        assert!(set_theme("astro"));
        if std::env::var_os("NO_COLOR").is_none() {
            assert_eq!(th().name, "astro");
        }
        assert!(set_theme("mono"));
        assert_eq!(th().name, "mono");
        assert!(!set_theme("arcoiris"));
        assert_eq!(th().name, "mono");
    }
}
