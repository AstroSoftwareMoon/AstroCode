//! Comandos slash.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Clear,
    /// `/model` sin argumento lista los modelos; con argumento cambia.
    Model(Option<String>),
    Tools,
    Cost,
    Context,
    Compact,
    Undo,
    Resume(Option<String>),
    Permissions(Option<String>),
    Init,
    Hardware,
    Install(Option<String>),
    Uninstall(Option<String>),
    Provider(Option<String>),
    Setup,
    Quit,
    Unknown(String),
}

pub const HELP: &str = "\
Comandos:
  /help                  esta ayuda
  /model [nombre]        lista los modelos o cambia de modelo (acepta proveedor/modelo)
  /provider [add|list]   gestiona proveedores (APIs y servidores locales)
  /hardware              tu hardware y los modelos que te caben
  /install [modelo]      instala un runtime o descarga un modelo recomendado
  /uninstall [modelo]    borra un modelo descargado
  /permissions [modo]    ask · auto-project · auto-all   (también Shift+Tab)
  /undo                  deshace el último turno (archivos y conversación)
  /resume [id]           reanuda una sesión guardada
  /compact               resume la conversación para liberar contexto
  /context               uso de la ventana de contexto
  /cost                  tokens y coste de la sesión
  /init                  crea un ASTRO.md analizando el proyecto
  /tools                 herramientas disponibles
  /setup                 asistente de configuración inicial
  /clear                 nueva conversación
  /quit                  salir (también Ctrl+D)

Teclas:
  Enter enviar · Alt+Enter / Ctrl+J nueva línea · Esc cancelar · Ctrl+C cancelar/salir
  ↑ ↓ historial · PgUp PgDn / rueda desplazar · Shift+Tab cambiar modo de permisos";

/// Nombres para autocompletar.
pub const NAMES: &[&str] = &[
    "help",
    "model",
    "provider",
    "hardware",
    "install",
    "uninstall",
    "permissions",
    "undo",
    "resume",
    "compact",
    "context",
    "cost",
    "init",
    "tools",
    "clear",
    "quit",
];

pub fn parse(input: &str) -> Option<Command> {
    let s = input.trim();
    let rest = s.strip_prefix('/')?;
    if rest.starts_with('/')
        || rest.contains('\n') && !rest.lines().next().unwrap_or("").starts_with(|c: char| c.is_alphabetic())
    {
        return None;
    }
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_lowercase();
    let arg = parts.next().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    Some(match name.as_str() {
        "help" | "h" | "?" => Command::Help,
        "clear" | "new" | "reset" => Command::Clear,
        "model" | "models" | "m" => Command::Model(arg),
        "tools" => Command::Tools,
        "cost" | "tokens" | "usage" => Command::Cost,
        "context" | "ctx" => Command::Context,
        "compact" | "summarize" => Command::Compact,
        "undo" | "u" => Command::Undo,
        "resume" | "sessions" | "continue" => Command::Resume(arg),
        "permissions" | "perms" | "mode" => Command::Permissions(arg),
        "init" => Command::Init,
        "hardware" | "hw" | "recommend" => Command::Hardware,
        "install" | "pull" => Command::Install(arg),
        "uninstall" | "rm" => Command::Uninstall(arg),
        "provider" | "providers" => Command::Provider(arg),
        "setup" | "onboarding" | "bienvenida" => Command::Setup,
        "quit" | "exit" | "q" => Command::Quit,
        _ => Command::Unknown(name),
    })
}

/// Sugerencia de autocompletado para lo escrito (sólo nombre de comando).
pub fn complete(input: &str) -> Option<&'static str> {
    let p = input.strip_prefix('/')?;
    if p.is_empty() || p.contains(char::is_whitespace) {
        return None;
    }
    NAMES.iter().copied().find(|n| n.starts_with(p) && *n != p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("hola"), None);
        assert_eq!(parse("/help"), Some(Command::Help));
        assert_eq!(parse("/model"), Some(Command::Model(None)));
        assert_eq!(parse("/model  qwen2.5-coder:7b "), Some(Command::Model(Some("qwen2.5-coder:7b".into()))));
        assert_eq!(parse("/QUIT"), Some(Command::Quit));
        assert_eq!(parse("/permissions auto-all"), Some(Command::Permissions(Some("auto-all".into()))));
        assert_eq!(parse("/resume 2026"), Some(Command::Resume(Some("2026".into()))));
        assert_eq!(parse("/foo"), Some(Command::Unknown("foo".into())));
        assert_eq!(parse("//no es comando"), None);
    }

    #[test]
    fn completes() {
        assert_eq!(complete("/he"), Some("help"));
        assert_eq!(complete("/hel"), Some("help"));
        assert_eq!(complete("/help"), None);
        assert_eq!(complete("/zz"), None);
        assert_eq!(complete("hola"), None);
    }
}
