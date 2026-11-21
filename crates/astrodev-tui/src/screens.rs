//! Pantallas modales.

use ratatui::Frame;

use crate::app::{App, Modal};

pub fn draw_modal(f: &mut Frame, app: &App) {
    match &app.modal {
        Some(Modal::Permission { .. }) => crate::ui::draw_permission(f, app),
        Some(Modal::Sessions { .. }) => crate::ui::draw_sessions(f, app),
        Some(Modal::Hardware(v)) => crate::hardware_view::draw(f, v),
        None => {}
    }
}
