//! `list_dir`: árbol del proyecto para que el modelo se oriente.

use std::collections::BTreeMap;
use std::path::Path;

use astrodev_core::tool::{arg_opt_str, arg_opt_u64, ToolEffect};
use astrodev_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

const MAX_ENTRIES: usize = 400;

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    is_dir: bool,
}

/// Construye un árbol textual de `base` hasta `depth` niveles.
pub fn tree(base: &Path, depth: usize) -> (String, usize) {
    let mut root = Node {
        is_dir: true,
        ..Default::default()
    };
    let mut count = 0;
    let walk = ignore::WalkBuilder::new(base)
        .hidden(true)
        .require_git(false)
        .max_depth(Some(depth))
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    for entry in walk.flatten() {
        let Ok(rel) = entry.path().strip_prefix(base) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_ENTRIES {
            break;
        }
        let mut node = &mut root;
        for comp in rel.components() {
            node = node
                .children
                .entry(comp.as_os_str().to_string_lossy().to_string())
                .or_default();
        }
        node.is_dir = entry.file_type().is_some_and(|t| t.is_dir());
    }
    let mut out = String::new();
    fn render(n: &Node, prefix: &str, out: &mut String) {
        // Directorios primero.
        let mut items: Vec<(&String, &Node)> = n.children.iter().collect();
        items.sort_by_key(|(name, node)| (!node.is_dir, (*name).clone()));
        let len = items.len();
        for (i, (name, child)) in items.into_iter().enumerate() {
            let last = i + 1 == len;
            out.push_str(prefix);
            out.push_str(if last { "└── " } else { "├── " });
            out.push_str(name);
            if child.is_dir {
                out.push('/');
            }
            out.push('\n');
            let next = format!("{prefix}{}", if last { "    " } else { "│   " });
            render(child, &next, out);
        }
    }
    render(&root, "", &mut out);
    (out, count)
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "Muestra el árbol de archivos de un directorio del proyecto \
                (respeta .gitignore)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directorio (por defecto la raíz)"},
                    "depth": {"type": "integer", "description": "Profundidad, por defecto 3"}
                }
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let base = match arg_opt_str(args, "path") {
            Some(p) if !p.trim().is_empty() && p != "." => {
                ctx.project.resolve(p, ctx.allow_outside_project)?
            }
            _ => ctx.project.root().to_path_buf(),
        };
        if !base.is_dir() {
            return Err(ToolError::NotFound(format!(
                "no es un directorio: {}",
                ctx.project.display(&base)
            )));
        }
        let depth = arg_opt_u64(args, "depth").unwrap_or(3).clamp(1, 10) as usize;
        let shown = ctx.project.display(&base);
        let (t, count) = tokio::task::spawn_blocking(move || tree(&base, depth))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let header = if shown.is_empty() {
            ".".to_string()
        } else {
            shown
        };
        let mut s = format!("{header}/\n{t}");
        if count > MAX_ENTRIES {
            s.push_str(&format!("[limitado a {MAX_ENTRIES} entradas]\n"));
        }
        Ok(ToolOutput::text(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::Project;
    use std::sync::Arc;

    #[tokio::test]
    async fn renders_tree() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/a")).unwrap();
        std::fs::write(d.path().join("src/a/x.rs"), "").unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
        let c = ToolContext::new(Arc::new(Project::new(d.path()).unwrap()));
        let out = ListDir.execute(&json!({}), &c).await.unwrap().content;
        assert!(out.starts_with("./\n"));
        assert!(out.contains("├── src/"));
        assert!(out.contains("└── Cargo.toml"));
        assert!(out.contains("x.rs"));
        let shallow = ListDir
            .execute(&json!({"depth": 1}), &c)
            .await
            .unwrap()
            .content;
        assert!(!shallow.contains("x.rs"));
    }
}
