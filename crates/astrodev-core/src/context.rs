//! Gestión de la ventana de contexto: estimación de tokens y compactación por resumen.

use crate::message::{Message, Role};

pub const SUMMARY_PREFIX: &str = "[Resumen de la conversación anterior]";

pub fn estimate(messages: &[Message]) -> usize {
    messages.iter().map(Message::estimated_tokens).sum()
}

/// Índice a partir del cual se conservan los mensajes recientes. Garantiza que el
/// tramo conservado empieza en un mensaje de usuario (no parte pares llamada/resultado).
/// Devuelve `None` si no hay nada que resumir.
pub fn split_point(messages: &[Message], keep_budget: usize) -> Option<usize> {
    if messages.len() < 3 {
        return None;
    }
    let mut acc = 0;
    let mut idx = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        acc += m.estimated_tokens();
        if acc > keep_budget && i < messages.len() - 1 {
            break;
        }
        idx = i;
    }
    // Avanzar/retroceder hasta un mensaje de usuario "real" (no resultado de herramienta).
    let starts_user = |i: usize| messages[i].role == Role::User;
    let mut cut = idx;
    while cut < messages.len() && !starts_user(cut) {
        cut += 1;
    }
    if cut >= messages.len() {
        // Sin usuario en el tramo reciente: retroceder al último mensaje de usuario.
        cut = (0..idx).rev().find(|i| starts_user(*i))?;
    }
    (cut > 0).then_some(cut)
}

/// Texto plano de los mensajes a resumir (los resultados largos se recortan).
pub fn transcript(messages: &[Message], max_tool_chars: usize) -> String {
    let mut s = String::new();
    for m in messages {
        let who = match m.role {
            Role::System => "SISTEMA",
            Role::User => "USUARIO",
            Role::Assistant => "ASISTENTE",
            Role::Tool => "RESULTADO",
        };
        let mut content = m.content.clone();
        if m.role == Role::Tool && content.len() > max_tool_chars {
            let mut cut = max_tool_chars;
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
            content.push_str(" …[recortado]");
        }
        s.push_str(&format!("{who}: {content}\n"));
        for c in &m.tool_calls {
            s.push_str(&format!("  → herramienta {} {}\n", c.name, c.arguments));
        }
    }
    s
}

pub fn summary_request(old: &[Message]) -> Vec<Message> {
    vec![
        Message::system(
            "Eres un asistente que resume conversaciones de programación para continuar el trabajo \
             con menos contexto. Conserva: objetivo del usuario, decisiones tomadas, archivos \
             tocados y su estado, errores pendientes y próximos pasos. Sé conciso (máx. 300 palabras), \
             usa viñetas y no inventes nada.",
        ),
        Message::user(format!(
            "Resume esta conversación:\n\n{}",
            transcript(old, 1500)
        )),
    ]
}

/// Sustituye `messages[..cut]` por el resumen.
pub fn apply_summary(messages: &mut Vec<Message>, cut: usize, summary: &str) {
    let rest = messages.split_off(cut);
    messages.clear();
    messages.push(Message::user(format!(
        "{SUMMARY_PREFIX}\n{}",
        summary.trim()
    )));
    messages.push(Message::assistant("Entendido. Continúo con ese contexto."));
    messages.extend(rest);
}

/// Compactación barata sin modelo: recorta resultados de herramientas antiguos.
pub fn shrink_old_tool_results(
    messages: &mut [Message],
    keep_last: usize,
    max_chars: usize,
) -> usize {
    let n = messages.len();
    let mut saved = 0;
    for m in messages.iter_mut().take(n.saturating_sub(keep_last)) {
        if m.role == Role::Tool && m.content.len() > max_chars {
            let before = m.content.len();
            let mut cut = max_chars;
            while !m.content.is_char_boundary(cut) {
                cut -= 1;
            }
            m.content.truncate(cut);
            m.content
                .push_str("\n…[resultado antiguo recortado para ahorrar contexto]");
            saved += before.saturating_sub(m.content.len());
        }
    }
    saved / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;

    fn convo() -> Vec<Message> {
        let call = ToolCall {
            id: "1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        vec![
            Message::user("primera petición ".repeat(50)),
            Message::assistant_with_tools("", vec![call.clone()]),
            Message::tool_result(&call, "x".repeat(4000)),
            Message::assistant("hecho"),
            Message::user("segunda petición"),
            Message::assistant_with_tools("", vec![call.clone()]),
            Message::tool_result(&call, "y".repeat(400)),
            Message::assistant("listo"),
        ]
    }

    #[test]
    fn split_starts_at_user() {
        let m = convo();
        let cut = split_point(&m, 200).unwrap();
        assert_eq!(cut, 4);
        assert_eq!(m[cut].role, Role::User);
    }

    #[test]
    fn small_conversations_not_split() {
        assert!(split_point(&convo()[..2], 10).is_none());
    }

    #[test]
    fn apply_summary_replaces_prefix() {
        let mut m = convo();
        apply_summary(&mut m, 4, "resumen");
        assert_eq!(m.len(), 6);
        assert!(m[0].content.starts_with(SUMMARY_PREFIX));
        assert_eq!(m[2].content, "segunda petición");
    }

    #[test]
    fn shrinks_old_results() {
        let mut m = convo();
        let before = estimate(&m);
        let saved = shrink_old_tool_results(&mut m, 4, 500);
        assert!(saved > 800);
        assert!(estimate(&m) < before);
        assert!(m[6].content.len() == 400, "los recientes no se tocan");
    }

    #[test]
    fn transcript_truncates() {
        let t = transcript(&convo(), 100);
        assert!(t.contains("[recortado]"));
        assert!(t.contains("→ herramienta read_file"));
    }
}
