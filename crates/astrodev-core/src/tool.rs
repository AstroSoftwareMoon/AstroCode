//! Trait de herramientas y registro.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::project::Project;

/// Descripción de una herramienta para el modelo (JSON Schema en `parameters`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Qué tipo de efecto tiene una herramienta: lo usa el sistema de permisos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    WritesFiles,
    RunsCommands,
}

#[derive(Clone)]
pub struct ToolContext {
    pub project: Arc<Project>,
    pub cancel: CancellationToken,
    /// Permite rutas fuera del proyecto (sólo tras permiso explícito del usuario).
    pub allow_outside_project: bool,
}

impl ToolContext {
    pub fn new(project: Arc<Project>) -> Self {
        Self {
            project,
            cancel: CancellationToken::new(),
            allow_outside_project: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolOutput {
    /// Texto que se devuelve al modelo.
    pub content: String,
    /// Diff unificado para mostrar al usuario (si la herramienta modificó archivos).
    pub diff: Option<String>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            diff: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("argumentos inválidos: {0}")]
    InvalidArgs(String),
    #[error("ruta fuera del proyecto: {0}")]
    OutsideProject(String),
    #[error("{0}")]
    NotFound(String),
    #[error("cancelado")]
    Cancelled,
    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Failed(String),
}

/// Lee un argumento string obligatorio.
pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs(format!("falta el parámetro string `{key}`")))
}

pub fn arg_opt_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn arg_opt_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

pub fn arg_opt_bool(args: &serde_json::Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| {
        v.as_bool().or_else(|| match v.as_str() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        })
    })
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn effect(&self) -> ToolEffect;

    /// Previsualización (p.ej. diff) antes de pedir permiso. `None` si no aplica.
    async fn preview(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<Option<String>, ToolError> {
        Ok(None)
    }

    /// Archivos que la herramienta modificaría (para checkpoints / `/undo`).
    fn affected_paths(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    async fn execute(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient_arg_parsing() {
        let a = json!({"n": "42", "b": "true", "s": "x"});
        assert_eq!(arg_opt_u64(&a, "n"), Some(42));
        assert_eq!(arg_opt_bool(&a, "b"), Some(true));
        assert_eq!(arg_str(&a, "s").unwrap(), "x");
        assert!(arg_str(&a, "missing").is_err());
    }
}
