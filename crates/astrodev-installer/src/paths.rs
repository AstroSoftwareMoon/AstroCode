//! Directorios gestionados por AstroDev.

use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    astrodev_core::config::data_dir().unwrap_or_else(|_| std::env::temp_dir().join("astrodev"))
}

/// Runtimes instalados por AstroDev (sin permisos de administrador).
pub fn runtimes_dir() -> PathBuf {
    data_dir().join("runtimes")
}

/// Modelos GGUF descargados para llama.cpp.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Busca un ejecutable en el PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let exe = exe(name);
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|p| p.join(&exe)).find(|p| p.is_file()))
}

/// Espacio libre en el disco que contiene `path`.
pub fn free_space(path: &std::path::Path) -> u64 {
    astrodev_hardware::detect::disk_free(path)
}
