//! `git`: ejecuta subcomandos de git en el proyecto (sin pasar por la shell).

use std::time::Duration;

use astrodev_core::tool::ToolEffect;
use astrodev_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::shell;

/// Subcomandos que no modifican el repositorio.
const READ_ONLY: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "blame",
    "branch",
    "rev-parse",
    "ls-files",
    "remote",
    "describe",
    "shortlog",
    "grep",
];

/// Extrae los argumentos: acepta `args` (array) o `args` (string separada por espacios).
pub fn parse_args(args: &Value) -> Result<Vec<String>, ToolError> {
    let v = args.get("args").ok_or_else(|| ToolError::InvalidArgs("falta `args`".into()))?;
    let out: Vec<String> = match v {
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        _ => return Err(ToolError::InvalidArgs("`args` debe ser array o string".into())),
    };
    let out: Vec<String> = if out.first().map(|s| s == "git").unwrap_or(false) { out[1..].to_vec() } else { out };
    if out.is_empty() {
        return Err(ToolError::InvalidArgs("`args` vacío".into()));
    }
    Ok(out)
}

/// ¿El subcomando es de sólo lectura? (`branch` con argumentos de borrado no lo es).
pub fn is_read_only(args: &[String]) -> bool {
    let Some(sub) = args.first() else {
        return false;
    };
    if !READ_ONLY.contains(&sub.as_str()) {
        return false;
    }
    let mutating_flags = ["-d", "-D", "-m", "-M", "--delete", "add", "remove", "set-url", "rename"];
    !(matches!(sub.as_str(), "branch" | "remote") && args.iter().skip(1).any(|a| mutating_flags.contains(&a.as_str())))
}

pub struct Git;

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git".into(),
            description: "Ejecuta git en el proyecto. Ejemplos de args: [\"status\"], \
                [\"diff\", \"--stat\"], [\"log\", \"-n\", \"5\", \"--oneline\"], \
                [\"commit\", \"-am\", \"mensaje\"]."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Argumentos para git (sin la palabra git)"
                    }
                },
                "required": ["args"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::RunsCommands
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let argv = parse_args(args)?;
        let mut cmd = Command::new("git");
        cmd.arg("--no-pager").args(&argv).env("GIT_TERMINAL_PROMPT", "0");
        let res = shell::run(cmd, ctx.project.root(), Duration::from_secs(120), &ctx.cancel).await?;
        Ok(ToolOutput::text(res.render()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_variants() {
        assert_eq!(parse_args(&json!({"args": ["status"]})).unwrap(), vec!["status"]);
        assert_eq!(parse_args(&json!({"args": "git log -n 3"})).unwrap(), vec!["log", "-n", "3"]);
        assert!(parse_args(&json!({"args": []})).is_err());
    }

    #[test]
    fn read_only_detection() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(is_read_only(&v("status")));
        assert!(is_read_only(&v("branch -a")));
        assert!(!is_read_only(&v("branch -D foo")));
        assert!(!is_read_only(&v("commit -m x")));
        assert!(!is_read_only(&v("push --force")));
    }

    #[tokio::test]
    async fn runs_git_status() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return; // git no disponible en este entorno
        }
        let d = tempfile::tempdir().unwrap();
        std::process::Command::new("git").arg("init").arg("-q").current_dir(d.path()).status().unwrap();
        let c = ToolContext::new(std::sync::Arc::new(astrodev_core::Project::new(d.path()).unwrap()));
        let out = Git.execute(&json!({"args": ["status"]}), &c).await.unwrap().content;
        assert!(out.contains("[código de salida: 0]"), "{out}");
    }
}
