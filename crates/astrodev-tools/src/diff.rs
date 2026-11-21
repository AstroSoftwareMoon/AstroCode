//! Diffs unificados para previsualizar cambios.

use similar::TextDiff;

/// Diff unificado con 3 líneas de contexto. Cadena vacía si no hay cambios.
pub fn unified(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// (líneas añadidas, líneas eliminadas)
pub fn stats(old: &str, new: &str) -> (usize, usize) {
    let d = TextDiff::from_lines(old, new);
    let mut add = 0;
    let mut del = 0;
    for change in d.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_unified_diff() {
        let d = unified("a.txt", "uno\ndos\n", "uno\nDOS\n");
        assert!(d.contains("--- a/a.txt"));
        assert!(d.contains("-dos"));
        assert!(d.contains("+DOS"));
        assert_eq!(stats("uno\ndos\n", "uno\nDOS\n"), (1, 1));
        assert!(unified("a", "x", "x").is_empty());
    }
}
