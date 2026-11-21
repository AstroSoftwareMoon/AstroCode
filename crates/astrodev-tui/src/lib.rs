//! # astrodev-tui
//!
//! Interfaz de terminal de AstroDev construida con ratatui + crossterm.

mod app;
mod commands;
mod hardware_view;
mod input;
mod install_view;
mod markdown;
mod provider_view;
mod screens;
mod theme;
mod ui;
mod wrap;

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use astrodev_core::Agent;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, Mutex};

pub use app::{App, AppMsg, ProviderFactory};

/// Datos de arranque que la TUI necesita del binario.
pub struct TuiOptions {
    pub agent: Agent,
    pub config: astrodev_core::Config,
    pub config_path: std::path::PathBuf,
    pub factory: ProviderFactory,
    /// `true` si el modelo no viene de la config/CLI y hay que elegir uno disponible.
    pub auto_select_model: bool,
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste);
}

/// Bucle principal de la TUI.
pub async fn run(opts: TuiOptions) -> anyhow::Result<()> {
    // Restaurar la terminal también si hay un pánico.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev(info);
    }));

    let mut term = setup_terminal()?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(Arc::new(Mutex::new(opts.agent)), opts.config, opts.config_path, opts.factory, tx).await;
    app.startup_checks(opts.auto_select_model);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let res: anyhow::Result<()> = async {
        loop {
            term.draw(|f| ui::draw(f, &mut app))?;
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => app.on_terminal_event(ev),
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(msg) = rx.recv() => app.on_msg(msg),
                _ = tick.tick() => app.tick(),
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    restore_terminal();
    res
}
