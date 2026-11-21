//! Lectura, escritura y edición de archivos.

use std::path::PathBuf;

use astrodev_core::tool::{arg_opt_bool, arg_opt_u64, arg_str, ToolEffect};
use astrodev_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::diff;

const DEFAULT_READ_LINES: u64 = 2000;
const MAX_LINE_CHARS: usize = 2000;

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Lee un archivo de texto del proyecto. Devuelve las líneas numeradas \
                (formato `N|contenido`). Usa offset/limit para archivos grandes."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Ruta relativa a la raíz del proyecto"},
                    "offset": {"type": "integer", "description": "Línea inicial (1 = primera)"},
                    "limit": {"type": "integer", "description": "Número máximo de líneas"}
                },
                "required": ["path"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let rel = arg_str(args, "path")?;
        let path = ctx.project.resolve(rel, ctx.allow_outside_project)?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| not_found_or_io(e, rel))?;
        if bytes.iter().take(8000).any(|b| *b == 0) {
            return Err(ToolError::Failed(format!(
                "{rel} parece un archivo binario"
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = arg_opt_u64(args, "offset").unwrap_or(1).max(1);
        let limit = arg_opt_u64(args, "limit")
            .unwrap_or(DEFAULT_READ_LINES)
            .max(1);
        let total = text.lines().count() as u64;
        let mut out = String::new();
        for (i, line) in text
            .lines()
            .enumerate()
            .skip((offset - 1) as usize)
            .take(limit as usize)
        {
            let line = if line.len() > MAX_LINE_CHARS {
                let mut cut = MAX_LINE_CHARS;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}…", &line[..cut])
            } else {
                line.to_string()
            };
            out.push_str(&format!("{:>5}|{}\n", i + 1, line));
        }
        if total == 0 {
            out.push_str("(archivo vacío)\n");
        }
        let shown_end = (offset - 1 + limit).min(total);
        if shown_end < total {
            out.push_str(&format!(
                "\n[mostrando líneas {offset}-{shown_end} de {total}; usa offset para seguir]\n"
            ));
        }
        Ok(ToolOutput::text(out))
    }
}

pub struct WriteFile;

impl WriteFile {
    async fn plan(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<(PathBuf, String, String), ToolError> {
        let rel = arg_str(args, "path")?;
        let content = arg_str(args, "content")?.to_string();
        let path = ctx.project.resolve(rel, ctx.allow_outside_project)?;
        let old = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        Ok((path, old, content))
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn affected_paths(&self, args: &Value, ctx: &ToolContext) -> Vec<PathBuf> {
        arg_str(args, "path")
            .ok()
            .and_then(|p| ctx.project.resolve(p, ctx.allow_outside_project).ok())
            .into_iter()
            .collect()
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Crea o sobrescribe un archivo completo. Para cambios parciales usa \
                edit_file. Crea los directorios intermedios necesarios."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Ruta relativa a la raíz del proyecto"},
                    "content": {"type": "string", "description": "Contenido completo del archivo"}
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::WritesFiles
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Result<Option<String>, ToolError> {
        let (path, old, new) = self.plan(args, ctx).await?;
        Ok(Some(diff::unified(&ctx.project.display(&path), &old, &new)))
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (path, old, new) = self.plan(args, ctx).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let existed = path.exists();
        tokio::fs::write(&path, &new).await?;
        let shown = ctx.project.display(&path);
        let d = diff::unified(&shown, &old, &new);
        let (add, del) = diff::stats(&old, &new);
        Ok(ToolOutput {
            content: format!(
                "{} {shown} (+{add} -{del})",
                if existed { "Sobrescrito" } else { "Creado" }
            ),
            diff: Some(d),
        })
    }
}

pub struct EditFile;

impl EditFile {
    /// Calcula (ruta, contenido antiguo, contenido nuevo) sin escribir nada.
    pub async fn plan(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<(PathBuf, String, String), ToolError> {
        let rel = arg_str(args, "path")?;
        let old_string = arg_str(args, "old_string")?;
        let new_string = arg_str(args, "new_string")?;
        let replace_all = arg_opt_bool(args, "replace_all").unwrap_or(false);
        let path = ctx.project.resolve(rel, ctx.allow_outside_project)?;

        if old_string.is_empty() {
            // Crear archivo nuevo con edit_file (old vacío) sólo si no existe o está vacío.
            let old = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            if !old.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "old_string vacío sólo se permite para crear archivos nuevos".into(),
                ));
            }
            return Ok((path, old, new_string.to_string()));
        }

        let old = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| not_found_or_io(e, rel))?;
        if old_string == new_string {
            return Err(ToolError::InvalidArgs(
                "old_string y new_string son iguales".into(),
            ));
        }
        let new = apply_edit(&old, old_string, new_string, replace_all)?;
        Ok((path, old, new))
    }
}

/// Aplica una edición búsqueda/reemplazo. Exige coincidencia única salvo `replace_all`.
/// Si no hay coincidencia exacta, intenta tolerar diferencias de fin de línea (CRLF) y de
/// espacios al final de cada línea — fallos habituales de modelos pequeños.
pub fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ToolError> {
    let count = content.matches(old).count();
    if count == 1 || (count > 1 && replace_all) {
        return Ok(if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        });
    }
    if count > 1 {
        return Err(ToolError::InvalidArgs(format!(
            "old_string aparece {count} veces; añade más contexto para que sea único o usa replace_all"
        )));
    }

    // CRLF: el archivo usa \r\n pero el modelo envió \n.
    if content.contains("\r\n") && !old.contains('\r') {
        let old_crlf = old.replace('\n', "\r\n");
        let new_crlf = new.replace('\n', "\r\n");
        if content.matches(&old_crlf).count() == 1 {
            return Ok(content.replacen(&old_crlf, &new_crlf, 1));
        }
    }

    // Coincidencia por líneas ignorando espacios finales.
    let norm = |s: &str| s.trim_end().to_string();
    let file_lines: Vec<&str> = content.split_inclusive('\n').collect();
    let old_lines: Vec<String> = old.lines().map(norm).collect();
    if !old_lines.is_empty() && old_lines.len() <= file_lines.len() {
        let mut hits = Vec::new();
        for start in 0..=file_lines.len() - old_lines.len() {
            if file_lines[start..start + old_lines.len()]
                .iter()
                .zip(&old_lines)
                .all(|(f, o)| norm(f) == *o)
            {
                hits.push(start);
            }
        }
        if hits.len() == 1 {
            let s = hits[0];
            let e = s + old_lines.len();
            let mut out: String = file_lines[..s].concat();
            out.push_str(new);
            let last = file_lines[e - 1];
            if last.ends_with('\n') && !new.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&file_lines[e..].concat());
            return Ok(out);
        }
    }

    Err(ToolError::InvalidArgs(
        "old_string no se encontró en el archivo; vuelve a leerlo con read_file y copia el texto exacto".into(),
    ))
}

#[async_trait]
impl Tool for EditFile {
    fn affected_paths(&self, args: &Value, ctx: &ToolContext) -> Vec<PathBuf> {
        arg_str(args, "path")
            .ok()
            .and_then(|p| ctx.project.resolve(p, ctx.allow_outside_project).ok())
            .into_iter()
            .collect()
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Edita un archivo sustituyendo `old_string` por `new_string`. \
                old_string debe ser texto exacto y único del archivo (incluye contexto). \
                Con old_string vacío crea un archivo nuevo."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string", "description": "Texto exacto a reemplazar"},
                    "new_string": {"type": "string", "description": "Texto nuevo"},
                    "replace_all": {"type": "boolean", "description": "Reemplazar todas las apariciones"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::WritesFiles
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Result<Option<String>, ToolError> {
        let (path, old, new) = self.plan(args, ctx).await?;
        Ok(Some(diff::unified(&ctx.project.display(&path), &old, &new)))
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (path, old, new) = self.plan(args, ctx).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &new).await?;
        let shown = ctx.project.display(&path);
        let (add, del) = diff::stats(&old, &new);
        Ok(ToolOutput {
            content: format!("Editado {shown} (+{add} -{del})"),
            diff: Some(diff::unified(&shown, &old, &new)),
        })
    }
}

pub(crate) fn not_found_or_io(e: std::io::Error, rel: &str) -> ToolError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ToolError::NotFound(format!("no existe: {rel}"))
    } else {
        ToolError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::Project;
    use std::sync::Arc;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "uno\ndos\ntres\n").unwrap();
        let p = Arc::new(Project::new(d.path()).unwrap());
        (d, ToolContext::new(p))
    }

    #[tokio::test]
    async fn read_numbers_lines_and_paginates() {
        let (_d, c) = ctx();
        let out = ReadFile
            .execute(&json!({"path": "a.txt"}), &c)
            .await
            .unwrap();
        assert!(out.content.contains("    1|uno"));
        assert!(out.content.contains("    3|tres"));
        let out = ReadFile
            .execute(&json!({"path": "a.txt", "offset": 2, "limit": 1}), &c)
            .await
            .unwrap();
        assert!(out.content.contains("    2|dos"));
        assert!(!out.content.contains("uno"));
        assert!(out.content.contains("de 3"));
    }

    #[tokio::test]
    async fn read_missing_and_outside() {
        let (_d, c) = ctx();
        assert!(matches!(
            ReadFile.execute(&json!({"path": "nope"}), &c).await,
            Err(ToolError::NotFound(_))
        ));
        assert!(matches!(
            ReadFile
                .execute(&json!({"path": "../../etc/passwd"}), &c)
                .await,
            Err(ToolError::OutsideProject(_))
        ));
    }

    #[tokio::test]
    async fn write_creates_dirs_and_diff() {
        let (d, c) = ctx();
        let out = WriteFile
            .execute(&json!({"path": "x/y/z.txt", "content": "hola\n"}), &c)
            .await
            .unwrap();
        assert!(out.content.starts_with("Creado x/y/z.txt"));
        assert!(out.diff.unwrap().contains("+hola"));
        assert_eq!(
            std::fs::read_to_string(d.path().join("x/y/z.txt")).unwrap(),
            "hola\n"
        );
    }

    #[tokio::test]
    async fn edit_preview_does_not_write() {
        let (d, c) = ctx();
        let args = json!({"path": "a.txt", "old_string": "dos", "new_string": "DOS"});
        let p = EditFile.preview(&args, &c).await.unwrap().unwrap();
        assert!(p.contains("+DOS"));
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "uno\ndos\ntres\n"
        );
        EditFile.execute(&args, &c).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.txt")).unwrap(),
            "uno\nDOS\ntres\n"
        );
    }

    #[test]
    fn edit_requires_unique() {
        let e = apply_edit("a a", "a", "b", false).unwrap_err().to_string();
        assert!(e.contains("2 veces"));
        assert_eq!(apply_edit("a a", "a", "b", true).unwrap(), "b b");
        assert!(apply_edit("abc", "zzz", "y", false).is_err());
    }

    #[test]
    fn edit_tolerates_crlf_and_trailing_spaces() {
        let crlf = "fn a() {\r\n    1\r\n}\r\n";
        let r = apply_edit(crlf, "    1\n}", "    2\n}", false).unwrap();
        assert_eq!(r, "fn a() {\r\n    2\r\n}\r\n");

        let spaced = "x = 1   \ny = 2\nz = 3\n";
        let r = apply_edit(spaced, "x = 1\ny = 2", "x = 10\ny = 20", false).unwrap();
        assert_eq!(r, "x = 10\ny = 20\nz = 3\n");
    }

    #[tokio::test]
    async fn edit_empty_old_creates_only_new_files() {
        let (d, c) = ctx();
        EditFile
            .execute(
                &json!({"path": "nuevo.rs", "old_string": "", "new_string": "fn x(){}"}),
                &c,
            )
            .await
            .unwrap();
        assert!(d.path().join("nuevo.rs").exists());
        assert!(EditFile
            .execute(
                &json!({"path": "a.txt", "old_string": "", "new_string": "x"}),
                &c
            )
            .await
            .is_err());
    }
}
