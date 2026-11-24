//! Proyecto actual: raíz, ASTRO.md y sandbox de rutas.

use std::path::{Component, Path, PathBuf};

use crate::tool::ToolError;

pub const CONTEXT_FILE: &str = "ASTRO.md";

#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    /// Directorio desde el que se abrió (puede ser un subdirectorio de la raíz).
    cwd: Option<PathBuf>,
}

impl Project {
    /// Usa `root` tal cual (canonicalizado).
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self { root: dunce_canonicalize(root.as_ref())?, cwd: None })
    }

    /// Busca hacia arriba desde `start` un directorio con `.git` o `ASTRO.md`.
    /// Si no lo encuentra, usa `start`.
    pub fn discover(start: impl AsRef<Path>) -> std::io::Result<Self> {
        let start = dunce_canonicalize(start.as_ref())?;
        // La raíz es el repositorio git más cercano; un ASTRO.md sólo marca la raíz si no hay
        // git (los ASTRO.md de subdirectorios son memoria adicional, no raíces).
        let mut first_astro: Option<PathBuf> = None;
        let mut cur: Option<&Path> = Some(&start);
        while let Some(dir) = cur {
            if dir.join(".git").exists() {
                return Ok(Self { root: dir.to_path_buf(), cwd: Some(start.clone()) });
            }
            if first_astro.is_none() && dir.join(CONTEXT_FILE).exists() {
                first_astro = Some(dir.to_path_buf());
            }
            cur = dir.parent();
        }
        let root = first_astro.unwrap_or_else(|| start.clone());
        Ok(Self { root, cwd: Some(start) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Contenido de `ASTRO.md` en la raíz del proyecto, si existe.
    pub fn context_file(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join(CONTEXT_FILE)).ok()
    }

    /// Resuelve `path` (relativo a la raíz o absoluto) y verifica que queda dentro del
    /// proyecto. Funciona también con rutas que aún no existen (para crear archivos) y
    /// resuelve symlinks del tramo existente, de modo que un enlace no permite escapar.
    pub fn resolve(&self, path: &str, allow_outside: bool) -> Result<PathBuf, ToolError> {
        let raw = Path::new(path.trim());
        let joined = if raw.is_absolute() { raw.to_path_buf() } else { self.root.join(raw) };
        let normalized = normalize_lexically(&joined);
        let resolved = canonicalize_existing_prefix(&normalized);
        if !allow_outside && !resolved.starts_with(&self.root) {
            return Err(ToolError::OutsideProject(path.to_string()));
        }
        Ok(resolved)
    }

    /// Ruta relativa a la raíz para mostrar.
    pub fn display(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.to_string_lossy().to_string())
    }
}

/// Elimina `.` y resuelve `..` sin tocar el sistema de archivos.
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicaliza el ancestro existente más profundo y le añade el resto.
fn canonicalize_existing_prefix(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = dunce_canonicalize(&existing) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.file_name().map(|s| s.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// `canonicalize` sin el prefijo `\\?\` de Windows, para que `starts_with` y la
/// visualización funcionen igual en todas las plataformas.
pub fn dunce_canonicalize(p: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            if !stripped.starts_with("UNC\\") {
                return Ok(PathBuf::from(stripped));
            }
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();
        let p = Project::new(dir.path()).unwrap();
        (dir, p)
    }

    #[test]
    fn resolves_inside() {
        let (_d, p) = setup();
        let r = p.resolve("src/main.rs", false).unwrap();
        assert!(r.starts_with(p.root()));
        assert_eq!(p.display(&r), "src/main.rs");
    }

    #[test]
    fn allows_new_files() {
        let (_d, p) = setup();
        let r = p.resolve("src/nuevo/mod.rs", false).unwrap();
        assert!(r.ends_with("src/nuevo/mod.rs"));
    }

    #[test]
    fn rejects_parent_escape() {
        let (_d, p) = setup();
        assert!(matches!(p.resolve("../../etc/passwd", false), Err(ToolError::OutsideProject(_))));
        assert!(p.resolve("src/../../x", false).is_err());
        assert!(p.resolve("../x", true).is_ok());
    }

    #[test]
    fn rejects_absolute_outside() {
        let (_d, p) = setup();
        let other = tempfile::tempdir().unwrap();
        let abs = other.path().join("f.txt");
        assert!(p.resolve(abs.to_str().unwrap(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let (d, p) = setup();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), d.path().join("link")).unwrap();
        assert!(p.resolve("link/secret.txt", false).is_err());
    }

    #[test]
    fn git_root_wins_over_subdir_astro_md() {
        let (d, _) = setup();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join("src/ASTRO.md"), "sub").unwrap();
        let p = Project::discover(d.path().join("src")).unwrap();
        assert_eq!(p.root(), dunce_canonicalize(d.path()).unwrap());
        assert_eq!(p.cwd(), Some(dunce_canonicalize(&d.path().join("src")).unwrap().as_path()));
    }

    #[test]
    fn discover_finds_marker() {
        let (d, _) = setup();
        std::fs::write(d.path().join(CONTEXT_FILE), "# ctx").unwrap();
        let p = Project::discover(d.path().join("src")).unwrap();
        assert_eq!(p.root(), dunce_canonicalize(d.path()).unwrap());
        assert_eq!(p.context_file().as_deref(), Some("# ctx"));
    }
}
