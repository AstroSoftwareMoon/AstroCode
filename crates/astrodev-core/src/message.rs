//! Mensajes de conversación independientes del proveedor.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Una llamada a herramienta emitida por el modelo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Identificador para emparejar con el resultado. Si el proveedor no lo da, se genera.
    pub id: String,
    pub name: String,
    /// Argumentos ya parseados como JSON.
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Para `Role::Tool`: id de la llamada a la que responde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Para `Role::Tool`: nombre de la herramienta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new(Role::Assistant, content)
        }
    }

    pub fn tool_result(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
            ..Self::new(Role::Tool, content)
        }
    }

    /// Estimación barata de tokens (≈ 4 caracteres por token). Suficiente para
    /// presupuestos de contexto cuando el proveedor no informa del uso real.
    pub fn estimated_tokens(&self) -> usize {
        let args: usize = self
            .tool_calls
            .iter()
            .map(|c| c.name.len() + c.arguments.to_string().len())
            .sum();
        (self.content.len() + args) / 4 + 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_links_call() {
        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "a.rs"}),
        };
        let m = Message::tool_result(&call, "ok");
        assert_eq!(m.role, Role::Tool);
        assert_eq!(m.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(m.name.as_deref(), Some("read_file"));
    }

    #[test]
    fn roundtrip_json() {
        let m = Message::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "x".into(),
                name: "glob".into(),
                arguments: serde_json::json!({"pattern": "*.rs"}),
            }],
        );
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
