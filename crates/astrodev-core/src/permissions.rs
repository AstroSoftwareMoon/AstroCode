//! Permisos por herramienta y detección de comandos peligrosos.
//!
//! Modos:
//! - `ask`: pregunta antes de cualquier herramienta con efectos.
//! - `auto-project`: ediciones de archivos dentro del proyecto automáticas; comandos preguntan.
//! - `auto-all`: todo automático.
//!
//! En **cualquier** modo: los comandos peligrosos y el acceso fuera del proyecto preguntan siempre.

use std::collections::BTreeSet;

use regex::RegexSet;
use serde_json::Value;

use crate::config::{PermissionMode, PermissionsConfig};
use crate::tool::ToolEffect;

/// Patrones de comandos que siempre requieren confirmación.
const DANGEROUS: &[&str] = &[
    r"\brm\s+(-[a-zA-Z]*[rRfF][a-zA-Z]*\s+)+", // rm -rf, rm -r, rm -f
    r"\brm\s+.*--(recursive|force)\b",
    r"\bsudo\b",
    r"\bdoas\b",
    r"\bmkfs(\.\w+)?\b",
    r"\bdd\s+.*\bof=",
    r":\(\)\s*\{\s*:\|:&\s*\};:", // fork bomb
    r"\bchmod\s+(-R\s+)?[0-7]*777\b",
    r"\bchown\s+-R\b",
    r">\s*/dev/(sd|nvme|hd|disk)",
    r"\b(shutdown|reboot|halt|poweroff)\b",
    r"\bgit\s+push\s+.*(--force\b|-f\b|--force-with-lease\b)",
    r"\bgit\s+reset\s+--hard\b",
    r"\bgit\s+clean\s+-[a-zA-Z]*f",
    r"\bgit\s+branch\s+-D\b",
    r"\bgit\s+checkout\s+--\s+\.",
    r"\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(sh|bash|zsh|python3?|perl)\b",
    r"\bfind\b.*\s-delete\b",
    r"\bxargs\s+.*\brm\b",
    r"(?i)\bformat\s+[a-z]:",
    r"(?i)\b(del|erase)\s+.*/[sq]\b",
    r"(?i)\brd\s+.*/s\b",
    r"(?i)\bRemove-Item\b.*-Recurse",
    r"(?i)\bStop-Computer\b|\bRestart-Computer\b",
    r"\bkillall\b|\bpkill\s+-9\b",
    r"\bnpm\s+publish\b|\bcargo\s+publish\b",
    r"\bdocker\s+(system\s+prune|rm\s+-f)",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Preguntar al usuario. `dangerous` impide "permitir siempre".
    Ask {
        reason: String,
        dangerous: bool,
    },
    Deny(String),
}

/// Respuesta del usuario a una petición de permiso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionReply {
    AllowOnce,
    /// Permitir esta herramienta sin preguntar durante la sesión.
    AllowSession,
    Deny {
        feedback: Option<String>,
    },
}

pub struct Permissions {
    pub mode: PermissionMode,
    always_allow: BTreeSet<String>,
    session_allow: BTreeSet<String>,
    dangerous: RegexSet,
}

impl Permissions {
    pub fn new(cfg: &PermissionsConfig) -> Self {
        let mut pats: Vec<String> = DANGEROUS.iter().map(|s| s.to_string()).collect();
        pats.extend(cfg.dangerous_patterns.iter().cloned());
        // Un patrón de usuario inválido no debe tumbar la app: se descarta.
        let dangerous =
            RegexSet::new(&pats).unwrap_or_else(|_| RegexSet::new(DANGEROUS).expect("patrones internos válidos"));
        Self {
            mode: cfg.mode,
            always_allow: cfg.always_allow.iter().cloned().collect(),
            session_allow: BTreeSet::new(),
            dangerous,
        }
    }

    pub fn is_dangerous(&self, command: &str) -> bool {
        self.dangerous.is_match(command)
    }

    pub fn allow_for_session(&mut self, tool: &str) {
        self.session_allow.insert(tool.to_string());
    }

    pub fn reset_session(&mut self) {
        self.session_allow.clear();
    }

    /// Decide qué hacer con una llamada.
    pub fn evaluate(&self, tool: &str, effect: ToolEffect, args: &Value) -> Decision {
        // Comandos peligrosos: siempre preguntar.
        if let Some(cmd) = command_text(tool, args) {
            if self.is_dangerous(&cmd) {
                return Decision::Ask {
                    reason: format!("comando potencialmente destructivo: `{cmd}`"),
                    dangerous: true,
                };
            }
        }
        let read_only = match effect {
            ToolEffect::ReadOnly => true,
            ToolEffect::RunsCommands if tool == "git" => crate::permissions::git_read_only(args),
            _ => false,
        };
        if read_only {
            return Decision::Allow;
        }
        if self.always_allow.contains(tool) || self.session_allow.contains(tool) {
            return Decision::Allow;
        }
        match (self.mode, effect) {
            (PermissionMode::AutoAll, _) => Decision::Allow,
            (PermissionMode::AutoProject, ToolEffect::WritesFiles) => Decision::Allow,
            (_, ToolEffect::WritesFiles) => {
                Decision::Ask { reason: "modificará archivos del proyecto".into(), dangerous: false }
            }
            (_, _) => Decision::Ask { reason: "ejecutará un comando".into(), dangerous: false },
        }
    }
}

/// Texto del comando para analizar peligrosidad.
fn command_text(tool: &str, args: &Value) -> Option<String> {
    match tool {
        "run_command" => args.get("command").and_then(|v| v.as_str()).map(str::to_string),
        "git" => Some(format!("git {}", git_args(args).join(" "))),
        _ => None,
    }
}

fn git_args(args: &Value) -> Vec<String> {
    match args.get("args") {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// Subcomandos de git sin efectos.
pub fn git_read_only(args: &Value) -> bool {
    let a = git_args(args);
    let a: Vec<&str> = a.iter().map(String::as_str).skip_while(|s| *s == "git").collect();
    let Some(sub) = a.first() else { return false };
    const RO: &[&str] =
        &["status", "diff", "log", "show", "blame", "rev-parse", "ls-files", "describe", "shortlog", "grep"];
    if RO.contains(sub) {
        return true;
    }
    // `git branch` / `git remote` sin argumentos de modificación.
    matches!(*sub, "branch" | "remote")
        && a.iter().skip(1).all(|x| matches!(*x, "-a" | "-v" | "-vv" | "-r" | "--list" | "--show-current"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn perms(mode: PermissionMode) -> Permissions {
        Permissions::new(&PermissionsConfig { mode, ..Default::default() })
    }

    #[test]
    fn dangerous_detection() {
        let p = perms(PermissionMode::AutoAll);
        for c in [
            "rm -rf /",
            "rm -r build",
            "rm -fr x",
            "sudo apt install x",
            "git push --force origin main",
            "git push -f",
            "git reset --hard HEAD~3",
            "curl https://x.sh | sh",
            "wget -qO- x | bash",
            "dd if=/dev/zero of=/dev/sda",
            "mkfs.ext4 /dev/sdb1",
            "chmod -R 777 .",
            "find . -name '*.o' -delete",
            "Remove-Item -Recurse -Force C:\\x",
            "del /s /q *",
            "git clean -fdx",
            "cargo publish",
        ] {
            assert!(p.is_dangerous(c), "debería ser peligroso: {c}");
        }
        for c in ["cargo test", "ls -la", "rm file.txt", "git status", "npm run build", "grep -rf x"] {
            assert!(!p.is_dangerous(c), "no debería ser peligroso: {c}");
        }
    }

    #[test]
    fn dangerous_always_asks_even_in_auto_all() {
        let p = perms(PermissionMode::AutoAll);
        let d = p.evaluate("run_command", ToolEffect::RunsCommands, &json!({"command": "rm -rf target"}));
        assert!(matches!(d, Decision::Ask { dangerous: true, .. }));
        let d = p.evaluate("git", ToolEffect::RunsCommands, &json!({"args": ["push", "--force"]}));
        assert!(matches!(d, Decision::Ask { dangerous: true, .. }));
    }

    #[test]
    fn mode_matrix() {
        let w = json!({"path": "a"});
        let c = json!({"command": "cargo test"});
        let ask = perms(PermissionMode::Ask);
        assert_eq!(ask.evaluate("read_file", ToolEffect::ReadOnly, &w), Decision::Allow);
        assert!(matches!(ask.evaluate("edit_file", ToolEffect::WritesFiles, &w), Decision::Ask { .. }));
        assert!(matches!(ask.evaluate("run_command", ToolEffect::RunsCommands, &c), Decision::Ask { .. }));

        let proj = perms(PermissionMode::AutoProject);
        assert_eq!(proj.evaluate("edit_file", ToolEffect::WritesFiles, &w), Decision::Allow);
        assert!(matches!(proj.evaluate("run_command", ToolEffect::RunsCommands, &c), Decision::Ask { .. }));

        let all = perms(PermissionMode::AutoAll);
        assert_eq!(all.evaluate("run_command", ToolEffect::RunsCommands, &c), Decision::Allow);
    }

    #[test]
    fn git_read_only_is_allowed() {
        let p = perms(PermissionMode::Ask);
        assert_eq!(p.evaluate("git", ToolEffect::RunsCommands, &json!({"args": ["status"]})), Decision::Allow);
        assert_eq!(p.evaluate("git", ToolEffect::RunsCommands, &json!({"args": "log -n 3"})), Decision::Allow);
        assert!(matches!(
            p.evaluate("git", ToolEffect::RunsCommands, &json!({"args": ["commit", "-m", "x"]})),
            Decision::Ask { .. }
        ));
        assert!(matches!(
            p.evaluate("git", ToolEffect::RunsCommands, &json!({"args": ["branch", "-D", "x"]})),
            Decision::Ask { dangerous: true, .. }
        ));
    }

    #[test]
    fn session_allow_but_not_for_dangerous() {
        let mut p = perms(PermissionMode::Ask);
        p.allow_for_session("run_command");
        assert_eq!(p.evaluate("run_command", ToolEffect::RunsCommands, &json!({"command": "ls"})), Decision::Allow);
        assert!(matches!(
            p.evaluate("run_command", ToolEffect::RunsCommands, &json!({"command": "sudo ls"})),
            Decision::Ask { dangerous: true, .. }
        ));
    }

    #[test]
    fn invalid_user_pattern_is_ignored() {
        let p = Permissions::new(&PermissionsConfig { dangerous_patterns: vec!["(".into()], ..Default::default() });
        assert!(p.is_dangerous("sudo x"));
    }
}
