//! Logs con niveles. En modo TUI nunca se escribe en la terminal: va a un archivo.
//! Cero telemetría: nada sale de la máquina.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

pub fn init(debug: bool, interactive: bool) -> Option<WorkerGuard> {
    let default = if debug { "debug" } else { "warn" };
    let filter = EnvFilter::try_from_env("ASTRODEV_LOG").unwrap_or_else(|_| EnvFilter::new(default));

    // En modo no interactivo con --debug, a stderr; si no, a archivo.
    if !interactive && debug {
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
        return None;
    }
    let dir = astrodev_core::config::data_dir().ok()?.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::daily(dir, "astrodev.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(writer).init();
    Some(guard)
}
