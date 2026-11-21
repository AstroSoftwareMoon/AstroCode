//! Búsqueda en el proyecto: `grep` (regex sobre contenido) y `glob` (nombres de archivo).
//! Ambas respetan `.gitignore` y omiten directorios ocultos.

use std::path::{Path, PathBuf};

use astrodev_core::tool::{arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str, ToolEffect};
use astrodev_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use async_trait::async_trait;
use globset::GlobMatcher;
use serde_json::{json, Value};

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .build()
}

fn compile_glob(p: &str) -> Result<GlobMatcher, ToolError> {
    // Un patrón sin '/' se aplica a cualquier profundidad.
    let p = if p.contains('/') || p.starts_with("**") {
        p.to_string()
    } else {
        format!("**/{p}")
    };
    globset::GlobBuilder::new(&p)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| ToolError::InvalidArgs(format!("glob inválido: {e}")))
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Busca una expresión regular en el contenido de los archivos del \
                proyecto. Devuelve `ruta:línea: texto`. Respeta .gitignore."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex (sintaxis Rust)"},
                    "path": {"type": "string", "description": "Subdirectorio o archivo (por defecto la raíz)"},
                    "glob": {"type": "string", "description": "Filtro de archivos, p.ej. *.rs"},
                    "case_insensitive": {"type": "boolean"},
                    "max_results": {"type": "integer", "description": "Por defecto 200"}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = arg_str(args, "pattern")?;
        let ci = arg_opt_bool(args, "case_insensitive").unwrap_or(false);
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(ci)
            .build()
            .map_err(|e| ToolError::InvalidArgs(format!("regex inválida: {e}")))?;
        let base = match arg_opt_str(args, "path") {
            Some(p) if !p.trim().is_empty() => ctx.project.resolve(p, ctx.allow_outside_project)?,
            _ => ctx.project.root().to_path_buf(),
        };
        let filter = arg_opt_str(args, "glob").map(compile_glob).transpose()?;
        let max = arg_opt_u64(args, "max_results").unwrap_or(200) as usize;
        let root = ctx.project.root().to_path_buf();
        let cancel = ctx.cancel.clone();

        let (lines, truncated) = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for entry in walker(&base).flatten() {
                if cancel.is_cancelled() {
                    break;
                }
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let path = entry.path();
                let r = rel(&root, path);
                if let Some(f) = &filter {
                    if !f.is_match(&r) {
                        continue;
                    }
                }
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                if bytes.iter().take(8000).any(|b| *b == 0) {
                    continue; // binario
                }
                let text = String::from_utf8_lossy(&bytes);
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let l: String = line.trim().chars().take(300).collect();
                        out.push(format!("{r}:{}: {l}", i + 1));
                        if out.len() >= max {
                            return (out, true);
                        }
                    }
                }
            }
            (out, false)
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?;

        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if lines.is_empty() {
            return Ok(ToolOutput::text(format!(
                "Sin coincidencias para `{pattern}`"
            )));
        }
        let mut s = lines.join("\n");
        if truncated {
            s.push_str(&format!(
                "\n[resultados limitados a {max}; refina la búsqueda]"
            ));
        }
        Ok(ToolOutput::text(s))
    }
}

pub struct Glob;

/// Lista archivos que coinciden con un glob (relativos a la raíz).
pub fn glob_files(root: &Path, pattern: &str, max: usize) -> Result<Vec<String>, ToolError> {
    let m = compile_glob(pattern)?;
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for entry in walker(root).flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            let r = rel(root, entry.path());
            if m.is_match(&r) {
                out.push((r, entry.path().to_path_buf()));
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    let mut names: Vec<String> = out.into_iter().map(|(r, _)| r).collect();
    names.sort();
    Ok(names)
}

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Encuentra archivos por patrón de nombre (p.ej. `**/*.rs`, `src/*.ts`). \
                Respeta .gitignore."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "max_results": {"type": "integer", "description": "Por defecto 500"}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = arg_str(args, "pattern")?.to_string();
        let max = arg_opt_u64(args, "max_results").unwrap_or(500) as usize;
        let root = ctx.project.root().to_path_buf();
        let files = tokio::task::spawn_blocking(move || glob_files(&root, &pattern, max))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        if files.is_empty() {
            return Ok(ToolOutput::text("Ningún archivo coincide"));
        }
        let n = files.len();
        let mut s = files.join("\n");
        if n >= max {
            s.push_str(&format!("\n[limitado a {max} resultados]"));
        }
        Ok(ToolOutput::text(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::Project;
    use std::sync::Arc;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::create_dir_all(p.join("src/sub")).unwrap();
        std::fs::create_dir_all(p.join("target")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main() {\n    hola();\n}\n").unwrap();
        std::fs::write(p.join("src/sub/lib.rs"), "pub fn hola() {}\n").unwrap();
        std::fs::write(p.join("README.md"), "Hola mundo\n").unwrap();
        std::fs::write(p.join("target/gen.rs"), "fn hola() {}\n").unwrap();
        std::fs::write(p.join(".gitignore"), "target/\n").unwrap();
        let c = ToolContext::new(Arc::new(Project::new(p).unwrap()));
        (d, c)
    }

    #[tokio::test]
    async fn grep_respects_gitignore_and_glob() {
        let (_d, c) = ctx();
        let out = Grep
            .execute(&json!({"pattern": "hola"}), &c)
            .await
            .unwrap()
            .content;
        assert!(out.contains("src/main.rs:2:"));
        assert!(out.contains("src/sub/lib.rs:1:"));
        assert!(!out.contains("target/"), "{out}");
        assert!(!out.contains("README"));

        let out = Grep
            .execute(
                &json!({"pattern": "hola", "case_insensitive": true, "glob": "*.md"}),
                &c,
            )
            .await
            .unwrap()
            .content;
        assert_eq!(out.trim(), "README.md:1: Hola mundo");
    }

    #[tokio::test]
    async fn grep_bad_regex() {
        let (_d, c) = ctx();
        assert!(matches!(
            Grep.execute(&json!({"pattern": "("}), &c).await,
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn glob_finds_files() {
        let (_d, c) = ctx();
        let out = Glob
            .execute(&json!({"pattern": "*.rs"}), &c)
            .await
            .unwrap()
            .content;
        assert_eq!(out, "src/main.rs\nsrc/sub/lib.rs");
        let out = Glob
            .execute(&json!({"pattern": "src/*.rs"}), &c)
            .await
            .unwrap()
            .content;
        assert_eq!(out, "src/main.rs");
    }
}
