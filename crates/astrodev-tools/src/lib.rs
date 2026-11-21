//! # astrodev-tools
//!
//! Herramientas que el agente puede invocar. Todas validan rutas contra la raíz del
//! proyecto mediante [`astrodev_core::Project::resolve`].

pub mod diff;
pub mod fs;
pub mod git;
pub mod list;
pub mod search;
pub mod shell;

use std::sync::Arc;

use astrodev_core::ToolRegistry;

/// Límite de caracteres devueltos al modelo por una herramienta.
pub const MAX_OUTPUT_CHARS: usize = 30_000;

/// Registro con todas las herramientas por defecto.
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(fs::ReadFile));
    r.register(Arc::new(fs::WriteFile));
    r.register(Arc::new(fs::EditFile));
    r.register(Arc::new(search::Grep));
    r.register(Arc::new(search::Glob));
    r.register(Arc::new(list::ListDir));
    r.register(Arc::new(shell::RunCommand));
    r.register(Arc::new(git::Git));
    r
}

/// Recorta texto largo conservando principio y final.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    let mut start = half;
    while !s.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = s.len() - half;
    while !s.is_char_boundary(end) {
        end += 1;
    }
    format!(
        "{}\n\n[... {} caracteres omitidos ...]\n\n{}",
        &s[..start],
        end - start,
        &s[end..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_tools() {
        let r = default_registry();
        for n in [
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "glob",
            "list_dir",
            "run_command",
            "git",
        ] {
            assert!(r.get(n).is_some(), "falta {n}");
        }
    }

    #[test]
    fn truncate_keeps_ends() {
        let s = "a".repeat(100) + &"b".repeat(100);
        let t = truncate_middle(&s, 50);
        assert!(t.starts_with("aaaa"));
        assert!(t.ends_with("bbbb"));
        assert!(t.contains("omitidos"));
        assert_eq!(truncate_middle("corto", 50), "corto");
        // No rompe caracteres multibyte.
        let u = "ñ".repeat(100);
        let _ = truncate_middle(&u, 51);
    }
}
