//! Colores y elementos visuales compartidos.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const ACCENT: Color = Color::Rgb(0x9B, 0x5C, 0xFF);
pub const PINK: Color = Color::Rgb(0xFF, 0x4F, 0xD8);
pub const CYAN: Color = Color::Rgb(0x3E, 0xC8, 0xFF);
pub const DIM: Color = Color::DarkGray;
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub const LOGO: &[&str] = &[
    r"    _        _             ____             ",
    r"   / \   ___| |_ _ __ ___ |  _ \  _____   __",
    r"  / _ \ / __| __| '__/ _ \| | | |/ _ \ \ / /",
    r" / ___ \\__ \ |_| | | (_) | |_| |  __/\ V / ",
    r"/_/   \_\___/\__|_|  \___/|____/ \___| \_/  ",
];

/// Colores de la marca: magenta → violeta → cian (igual que assets/logo.svg).
const BRAND: [(u8, u8, u8); 3] = [(0xFF, 0x4F, 0xD8), (0x9B, 0x5C, 0xFF), (0x3E, 0xC8, 0xFF)];

pub fn brand_color(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b, u) = if t < 0.55 { (BRAND[0], BRAND[1], t / 0.55) } else { (BRAND[1], BRAND[2], (t - 0.55) / 0.45) };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

pub fn gradient_line(s: &str) -> Line<'static> {
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
        Span::styled("░".repeat(width - full), Style::default().fg(DIM)),
    ]
}

pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}
