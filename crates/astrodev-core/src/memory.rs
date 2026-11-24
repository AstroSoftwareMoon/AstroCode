//! Memoria del proyecto al estilo de `CLAUDE.md`: archivos `ASTRO.md` que se añaden
//! al contexto de cada conversación.
//!
//! Se cargan, en este orden (lo más específico al final, para que prevalezca):
//!
//! 1. **Global** — `~/.config/astrodev/ASTRO.md`: tus preferencias en todos los proyectos.
//! 2. **Proyecto** — `<raíz>/ASTRO.md` (o `<raíz>/.astrodev/ASTRO.md`): se comparte en git.
//! 3. **Subdirectorios** — `ASTRO.md` de las carpetas entre la raíz y el directorio actual.
//! 4. **Personal** — `<raíz>/ASTRO.local.md`: notas tuyas para este proyecto, fuera de git.
//!
//! Dentro de cualquiera, una línea `@ruta/a/archivo.md` importa ese archivo
//! (relativo al que lo importa, `~` para tu home; hasta 5 niveles, sin ciclos).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::project::{Project, CONTEXT_FILE};

pub const LOCAL_FILE: &str = "ASTRO.local.md";
const MAX_IMPORT_DEPTH: usize = 5;
const MAX_FILE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    Global,
    Project,
    Subdir,
    Local,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            MemoryKind::Global => "global",
            MemoryKind::Project => "proyecto",
            MemoryKind::Subdir => "subdirectorio",
            MemoryKind::Local => "personal",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryFile {
    pub kind: MemoryKind,
    pub path: PathBuf,
    /// Contenido con los `@imports` ya resueltos.
    pub content: String,
    /// Archivos importados (para mostrarlos en `/memory`).
    pub imports: Vec<PathBuf>,
}

pub fn global_path() -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|d| d.join(CONTEXT_FILE))
}

pub fn project_path(project: &Project) -> PathBuf {
    let alt = project.root().join(".astrodev").join(CONTEXT_FILE);
    if !project.root().join(CONTEXT_FILE).exists() && alt.exists() {
        alt
    } else {
        project.root().join(CONTEXT_FILE)
    }
}

pub fn local_path(project: &Project) -> PathBuf {
    project.root().join(LOCAL_FILE)
}

/// Carga todos los archivos de memoria aplicables. `cwd` es el directorio de trabajo
/// (puede ser un subdirectorio del proyecto).
pub fn load(project: &Project, cwd: Option<&Path>) -> Vec<MemoryFile> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |kind, path: PathBuf, out: &mut Vec<MemoryFile>| {
        let canon = crate::project::dunce_canonicalize(&path).unwrap_or(path.clone());
        if !seen.insert(canon) {
            return;
        }
        if let Some(f) = read_with_imports(kind, &path) {
            out.push(f);
        }
    };
    if let Some(g) = global_path() {
        push(MemoryKind::Global, g, &mut out);
    }
    push(MemoryKind::Project, project_path(project), &mut out);
    if let Some(cwd) = cwd {
        // Normaliza igual que la raíz (sin `\\?\` en Windows) para que strip_prefix funcione.
        let cwd = crate::project::dunce_canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        if let Ok(rel) = cwd.strip_prefix(project.root()) {
            let mut dir = project.root().to_path_buf();
            for comp in rel.components() {
                dir.push(comp);
                push(MemoryKind::Subdir, dir.join(CONTEXT_FILE), &mut out);
            }
        }
    }
    push(MemoryKind::Local, local_path(project), &mut out);
    out
}

fn read_with_imports(kind: MemoryKind, path: &Path) -> Option<MemoryFile> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let mut imports = Vec::new();
    let mut stack = vec![std::fs::canonicalize(path).unwrap_or(path.to_path_buf())];
    let content = expand(&raw, path.parent().unwrap_or(Path::new(".")), 0, &mut stack, &mut imports);
    Some(MemoryFile { kind, path: path.to_path_buf(), content, imports })
}

fn resolve_import(token: &str, base: &Path) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new() {
            return home.home_dir().join(rest);
        }
    }
    let p = Path::new(token);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Sustituye las líneas `@ruta` por el contenido del archivo (fuera de bloques de código).
fn expand(text: &str, base: &Path, depth: usize, stack: &mut Vec<PathBuf>, imports: &mut Vec<PathBuf>) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_code = !in_code;
        }
        let token = t.strip_prefix('@').filter(|r| !in_code && !r.is_empty() && !r.contains(char::is_whitespace));
        match token {
            Some(tok) if depth < MAX_IMPORT_DEPTH => {
                let p = resolve_import(tok, base);
                let canon = std::fs::canonicalize(&p).unwrap_or(p.clone());
                if stack.contains(&canon) {
                    out.push_str(&format!("<!-- import circular omitido: {tok} -->\n"));
                    continue;
                }
                match std::fs::read_to_string(&p) {
                    Ok(mut c) => {
                        c.truncate(floor_char(&c, MAX_FILE_BYTES));
                        imports.push(p.clone());
                        stack.push(canon);
                        let inner = expand(&c, p.parent().unwrap_or(base), depth + 1, stack, imports);
                        stack.pop();
                        out.push_str(&format!("<!-- importado de {tok} -->\n{}\n", inner.trim_end()));
                    }
                    Err(_) => out.push_str(&format!("{line}\n")),
                }
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn floor_char(s: &str, max: usize) -> usize {
    let mut n = max.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// Texto para el prompt de sistema.
pub fn render(files: &[MemoryFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n# Memoria (ASTRO.md)\nInstrucciones del usuario y del proyecto. Síguelas; si se contradicen, \
         prevalece la más específica (las últimas).\n",
    );
    for f in files {
        s.push_str(&format!(
            "\n<astro_md tipo=\"{}\" ruta=\"{}\">\n{}\n</astro_md>\n",
            f.kind.label(),
            f.path.display(),
            f.content.trim()
        ));
    }
    s
}

/// Añade una nota (atajo `#` en la TUI) al final de un archivo de memoria.
pub fn append_note(path: &Path, note: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cur = std::fs::read_to_string(path).unwrap_or_default();
    if cur.is_empty() {
        cur.push_str("# Memoria\n\n");
    } else if !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(&format!("- {}\n", note.trim()));
    std::fs::write(path, cur)
}

/// Asegura que `ASTRO.local.md` está en el `.gitignore` del proyecto (si es un repo git).
pub fn ignore_local(project: &Project) -> std::io::Result<bool> {
    if !project.root().join(".git").exists() {
        return Ok(false);
    }
    let gi = project.root().join(".gitignore");
    let cur = std::fs::read_to_string(&gi).unwrap_or_default();
    if cur.lines().any(|l| l.trim() == LOCAL_FILE) {
        return Ok(false);
    }
    let mut new = cur;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(LOCAL_FILE);
    new.push('\n');
    std::fs::write(gi, new)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj() -> (tempfile::TempDir, Project) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        let p = Project::new(d.path()).unwrap();
        (d, p)
    }

    #[test]
    fn loads_project_subdir_and_local_in_order() {
        let (d, p) = proj();
        std::fs::write(d.path().join("ASTRO.md"), "raíz").unwrap();
        std::fs::create_dir_all(d.path().join("web/src")).unwrap();
        std::fs::write(d.path().join("web/ASTRO.md"), "frontend").unwrap();
        std::fs::write(d.path().join("ASTRO.local.md"), "mío").unwrap();
        let cwd = std::fs::canonicalize(d.path().join("web/src")).unwrap();
        let files = load(&p, Some(&cwd));
        let kinds: Vec<_> = files
            .iter()
            .filter(|f| f.kind != MemoryKind::Global)
            .map(|f| (f.kind, f.content.trim().to_string()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (MemoryKind::Project, "raíz".into()),
                (MemoryKind::Subdir, "frontend".into()),
                (MemoryKind::Local, "mío".into())
            ]
        );
        let r = render(&files);
        assert!(r.contains("tipo=\"personal\""));
        assert!(r.find("raíz").unwrap() < r.find("mío").unwrap());
    }

    #[test]
    fn imports_are_expanded_with_cycle_protection() {
        let (d, p) = proj();
        std::fs::create_dir_all(d.path().join("docs")).unwrap();
        std::fs::write(d.path().join("ASTRO.md"), "# Proyecto\n@docs/estilo.md\n```\n@no-importar.md\n```\n").unwrap();
        std::fs::write(d.path().join("docs/estilo.md"), "Usa tabs.\n@../ASTRO.md\n@otro.md").unwrap();
        std::fs::write(d.path().join("docs/otro.md"), "Commits en español.").unwrap();
        let f = &load(&p, None).into_iter().find(|f| f.kind == MemoryKind::Project).unwrap();
        assert!(f.content.contains("Usa tabs."));
        assert!(f.content.contains("Commits en español."), "imports relativos al archivo que importa");
        assert!(f.content.contains("import circular omitido"));
        assert!(f.content.contains("@no-importar.md"), "dentro de bloques de código no se importa");
        assert_eq!(f.imports.len(), 2);
    }

    #[test]
    fn quick_note_and_gitignore() {
        let (d, p) = proj();
        let path = project_path(&p);
        append_note(&path, "usa pnpm, no npm").unwrap();
        append_note(&path, "los tests van en tests/").unwrap();
        let c = std::fs::read_to_string(&path).unwrap();
        assert!(c.starts_with("# Memoria"));
        assert!(c.contains("- usa pnpm, no npm\n- los tests van en tests/\n"));
        assert!(ignore_local(&p).unwrap());
        assert!(!ignore_local(&p).unwrap(), "no se duplica");
        assert!(std::fs::read_to_string(d.path().join(".gitignore")).unwrap().contains(LOCAL_FILE));
    }

    #[test]
    fn empty_files_are_skipped_and_alt_location_works() {
        let (d, p) = proj();
        std::fs::write(d.path().join("ASTRO.md"), "  \n").unwrap();
        assert!(load(&p, None).iter().all(|f| f.kind != MemoryKind::Project));
        std::fs::remove_file(d.path().join("ASTRO.md")).unwrap();
        std::fs::create_dir_all(d.path().join(".astrodev")).unwrap();
        std::fs::write(d.path().join(".astrodev/ASTRO.md"), "oculto").unwrap();
        assert!(project_path(&p).ends_with(".astrodev/ASTRO.md"));
    }
}
