//! Comandos slash.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Clear,
    /// `/model` sin argumento lista los modelos; con argumento cambia.
    Model(Option<String>),
    Tools,
    Cost,
    Quit,
    /// Comandos previstos en fases posteriores.
    Planned {
        name: String,
        phase: u8,
    },
    Unknown(String),
}

pub const HELP: &str = "\
Comandos:
  /help              esta ayuda
  /model [nombre]    lista los modelos del proveedor o cambia de modelo
  /tools             herramientas disponibles para el modelo
  /cost              tokens consumidos en la sesión
  /clear             borra la conversación
  /quit              salir (también Ctrl+D)

Teclas:
  Enter              enviar          Alt+Enter / Ctrl+J  nueva línea
  Esc                cancelar respuesta en curso
  Ctrl+C             cancelar / salir
  ↑ ↓                historial       PgUp PgDn / rueda  desplazar
  Ctrl+W / Ctrl+U    borrar palabra / línea";

pub fn parse(input: &str) -> Option<Command> {
    let s = input.trim();
    let rest = s.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_lowercase();
    let arg = parts
        .next()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    Some(match name.as_str() {
        "help" | "h" | "?" => Command::Help,
        "clear" | "new" => Command::Clear,
        "model" | "models" => Command::Model(arg),
        "tools" => Command::Tools,
        "cost" | "tokens" => Command::Cost,
        "quit" | "exit" | "q" => Command::Quit,
        "undo" | "resume" | "sessions" | "permissions" => Command::Planned { name, phase: 2 },
        "hardware" | "recommend" => Command::Planned { name, phase: 3 },
        "install" | "uninstall" | "pull" => Command::Planned { name, phase: 4 },
        "provider" => Command::Planned { name, phase: 5 },
        _ => Command::Unknown(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("hola"), None);
        assert_eq!(parse("/help"), Some(Command::Help));
        assert_eq!(parse("/model"), Some(Command::Model(None)));
        assert_eq!(
            parse("/model  qwen2.5-coder:7b "),
            Some(Command::Model(Some("qwen2.5-coder:7b".into())))
        );
        assert_eq!(parse("/QUIT"), Some(Command::Quit));
        assert!(matches!(
            parse("/hardware"),
            Some(Command::Planned { phase: 3, .. })
        ));
        assert_eq!(parse("/foo"), Some(Command::Unknown("foo".into())));
    }
}
