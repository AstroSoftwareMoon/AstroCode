//! `run_command`: ejecuta un comando de shell en el proyecto con timeout y cancelación.

use std::process::Stdio;
use std::time::Duration;

use astrodev_core::tool::{arg_opt_str, arg_opt_u64, arg_str, ToolEffect};
use astrodev_core::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::{truncate_middle, MAX_OUTPUT_CHARS};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 1800;

/// Construye el comando de shell nativo de la plataforma.
pub fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CommandResult {
    pub fn render(&self) -> String {
        let mut s = String::new();
        if !self.stdout.is_empty() {
            s.push_str(&self.stdout);
            if !self.stdout.ends_with('\n') {
                s.push('\n');
            }
        }
        if !self.stderr.is_empty() {
            s.push_str("[stderr]\n");
            s.push_str(&self.stderr);
            if !self.stderr.ends_with('\n') {
                s.push('\n');
            }
        }
        if self.timed_out {
            s.push_str("[tiempo agotado: proceso terminado]\n");
        } else {
            match self.exit_code {
                Some(c) => s.push_str(&format!("[código de salida: {c}]")),
                None => s.push_str("[terminado por señal]"),
            }
        }
        truncate_middle(&s, MAX_OUTPUT_CHARS)
    }
}

/// Ejecuta `cmd` en `cwd` respetando timeout y cancelación.
pub async fn run(
    mut command: Command,
    cwd: &std::path::Path,
    timeout: Duration,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CommandResult, ToolError> {
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // En Unix creamos un grupo de procesos para poder matar también a los nietos
    // (p.ej. `sh -c "sleep 100"`), que si no mantendrían abiertas las tuberías.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| ToolError::Failed(format!("no se pudo lanzar el comando: {e}")))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let out_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b).await;
        b
    });
    let err_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b).await;
        b
    });

    let (status, timed_out) = tokio::select! {
        s = child.wait() => (Some(s?), false),
        _ = tokio::time::sleep(timeout) => {
            kill_tree(&mut child).await;
            (None, true)
        }
        _ = cancel.cancelled() => {
            kill_tree(&mut child).await;
            return Err(ToolError::Cancelled);
        }
    };
    let stdout = String::from_utf8_lossy(&out_task.await.unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&err_task.await.unwrap_or_default()).to_string();
    Ok(CommandResult {
        exit_code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
    })
}

async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: kill(2) con un pgid negativo envía la señal a todo el grupo.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "Ejecuta un comando de shell en el directorio del proyecto (sh en \
                Unix, cmd en Windows). Úsalo para compilar, ejecutar tests, etc. No interactivo."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "cwd": {"type": "string", "description": "Subdirectorio del proyecto"},
                    "timeout_secs": {"type": "integer", "description": "Por defecto 120"}
                },
                "required": ["command"]
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::RunsCommands
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let cmd = arg_str(args, "command")?;
        if cmd.trim().is_empty() {
            return Err(ToolError::InvalidArgs("comando vacío".into()));
        }
        let cwd = match arg_opt_str(args, "cwd") {
            Some(p) if !p.trim().is_empty() => ctx.project.resolve(p, ctx.allow_outside_project)?,
            _ => ctx.project.root().to_path_buf(),
        };
        let timeout = arg_opt_u64(args, "timeout_secs")
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);
        tracing::debug!(%cmd, "run_command");
        let res = run(
            shell_command(cmd),
            &cwd,
            Duration::from_secs(timeout),
            &ctx.cancel,
        )
        .await?;
        Ok(ToolOutput::text(res.render()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use astrodev_core::Project;
    use std::sync::Arc;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(Arc::new(Project::new(d.path()).unwrap()));
        (d, c)
    }

    #[tokio::test]
    async fn runs_in_project_root() {
        let (d, c) = ctx();
        std::fs::write(d.path().join("f.txt"), "x").unwrap();
        let out = RunCommand
            .execute(&json!({"command": "ls && echo err >&2; exit 3"}), &c)
            .await
            .unwrap()
            .content;
        assert!(out.contains("f.txt"));
        assert!(out.contains("[stderr]\nerr"));
        assert!(out.contains("[código de salida: 3]"));
    }

    #[tokio::test]
    async fn times_out() {
        let (_d, c) = ctx();
        let out = RunCommand
            .execute(&json!({"command": "sleep 5", "timeout_secs": 1}), &c)
            .await
            .unwrap()
            .content;
        assert!(out.contains("tiempo agotado"));
    }

    #[tokio::test]
    async fn cancels() {
        let (_d, c) = ctx();
        let cancel = c.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let r = RunCommand.execute(&json!({"command": "sleep 5"}), &c).await;
        assert!(matches!(r, Err(ToolError::Cancelled)));
    }

    #[tokio::test]
    async fn cwd_must_be_inside() {
        let (_d, c) = ctx();
        let r = RunCommand
            .execute(&json!({"command": "ls", "cwd": "../.."}), &c)
            .await;
        assert!(matches!(r, Err(ToolError::OutsideProject(_))));
    }
}
