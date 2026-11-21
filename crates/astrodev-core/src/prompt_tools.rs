//! Tool calling "por prompt" para modelos sin soporte nativo.
//!
//! El modelo escribe llamadas en su texto con este formato:
//!
//! ```text
//! <tool_call>
//! {"name": "read_file", "arguments": {"path": "src/main.rs"}}
//! </tool_call>
//! ```
//!
//! El parser es tolerante: acepta bloques ```json, JSON suelto con `name`/`arguments`,
//! claves alternativas (`tool`, `function`, `args`, `parameters`, `input`), argumentos
//! como string JSON, comas finales, comillas tipográficas, saltos de línea sin escapar
//! dentro de strings, llaves sin cerrar y una variante XML
//! (`<tool_call><name>x</name><arguments>{…}</arguments></tool_call>`).

use serde_json::{Map, Value};

use crate::message::{Message, Role, ToolCall};
use crate::tool::ToolSpec;

pub const OPEN: &str = "<tool_call>";
pub const CLOSE: &str = "</tool_call>";

/// Instrucciones que se añaden al prompt de sistema.
pub fn instructions(tools: &[ToolSpec]) -> String {
    let mut s = String::from(
        "\n# Uso de herramientas\n\
         Para usar una herramienta escribe EXACTAMENTE este formato (JSON válido, una llamada por bloque):\n\
         <tool_call>\n{\"name\": \"NOMBRE\", \"arguments\": {\"param\": \"valor\"}}\n</tool_call>\n\
         Después de escribir una o varias llamadas, DETENTE y espera: recibirás los resultados en \
         <tool_result>. No inventes resultados. Cuando no necesites más herramientas, responde normalmente \
         sin ningún bloque <tool_call>.\n\nHerramientas:\n",
    );
    for t in tools {
        let params = t
            .parameters
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| {
                let req: Vec<&str> = t
                    .parameters
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                p.iter()
                    .map(|(k, v)| {
                        let ty = v.get("type").and_then(Value::as_str).unwrap_or("any");
                        let opt = if req.contains(&k.as_str()) { "" } else { "?" };
                        format!("{k}{opt}: {ty}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        s.push_str(&format!("- {}({params}): {}\n", t.name, t.description));
    }
    s
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// Texto sin los bloques de llamada.
    pub text: String,
    pub calls: Vec<ToolCall>,
    /// Errores de bloques que parecían llamadas pero no se pudieron interpretar.
    pub errors: Vec<String>,
}

/// Extrae llamadas del texto del modelo. `known` limita los nombres aceptados para el
/// modo de "JSON suelto" (evita confundir JSON normal con llamadas).
pub fn parse(text: &str, known: &[String]) -> Parsed {
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    let mut clean = String::new();
    let mut rest = text;

    // 1) Bloques <tool_call>…</tool_call> (el cierre puede faltar al final).
    while let Some(start) = rest.find(OPEN) {
        clean.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let (body, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            None => (after, ""),
        };
        match parse_block(body) {
            Ok(c) => calls.push(c),
            Err(e) => errors.push(e),
        }
        rest = next;
    }
    clean.push_str(rest);

    // 2) Si no hubo bloques, buscar ```json … ``` o JSON suelto con un nombre conocido.
    if calls.is_empty() && errors.is_empty() {
        let mut out = String::new();
        let mut found = false;
        for (is_code, chunk) in split_fences(&clean) {
            if is_code {
                if let Ok(c) = parse_block(&chunk) {
                    if known.contains(&c.name) {
                        calls.push(c);
                        found = true;
                        continue;
                    }
                }
                out.push_str("```");
                out.push_str(&chunk);
                out.push_str("```");
            } else {
                let mut remaining = chunk.as_str();
                let mut kept = String::new();
                while let Some((s, e)) = find_json_object(remaining) {
                    let candidate = &remaining[s..e];
                    match parse_block(candidate) {
                        Ok(c) if known.contains(&c.name) => {
                            kept.push_str(&remaining[..s]);
                            calls.push(c);
                            found = true;
                        }
                        _ => kept.push_str(&remaining[..e]),
                    }
                    remaining = &remaining[e..];
                }
                kept.push_str(remaining);
                out.push_str(&kept);
            }
        }
        if found {
            clean = out;
        }
    }

    for (i, c) in calls.iter_mut().enumerate() {
        if c.id.is_empty() {
            c.id = format!("pt_{}_{i}", std::process::id());
        }
    }
    Parsed { text: clean.trim().to_string(), calls, errors }
}

/// Divide en trozos (es_código, contenido) según bloques ``` (quitando la etiqueta de lenguaje).
fn split_fences(s: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut parts = s.split("```");
    let mut code = false;
    if let Some(first) = parts.next() {
        out.push((false, first.to_string()));
    }
    for p in parts {
        code = !code;
        if code {
            // quitar "json\n"
            let body = match p.find('\n') {
                Some(i) if !p[..i].trim().contains('{') => &p[i + 1..],
                _ => p,
            };
            out.push((true, body.to_string()));
        } else {
            out.push((false, p.to_string()));
        }
    }
    out
}

/// Primer objeto JSON equilibrado `{…}` (respetando strings).
fn find_json_object(s: &str) -> Option<(usize, usize)> {
    let start = s.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in s[start..].char_indices() {
        if in_str {
            match ch {
                '\\' if !esc => esc = true,
                '"' if !esc => in_str = false,
                _ => esc = false,
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, start + i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_block(body: &str) -> Result<ToolCall, String> {
    let b = body.trim();
    let b = b.strip_prefix("```json").or_else(|| b.strip_prefix("```")).unwrap_or(b).trim_end_matches("```").trim();
    if b.starts_with('<') {
        return parse_xml(b);
    }
    let v = lenient_json(b).ok_or_else(|| format!("JSON inválido en la llamada: {}", preview(b)))?;
    call_from_value(v)
}

fn call_from_value(v: Value) -> Result<ToolCall, String> {
    let obj = match v {
        Value::Object(o) => o,
        Value::Array(mut a) if a.len() == 1 => match a.remove(0) {
            Value::Object(o) => o,
            _ => return Err("se esperaba un objeto".into()),
        },
        _ => return Err("se esperaba un objeto JSON {\"name\", \"arguments\"}".into()),
    };
    // {"function": {"name":…, "arguments":…}}
    if let Some(Value::Object(f)) = obj.get("function") {
        return call_from_value(Value::Object(f.clone()));
    }
    let name = ["name", "tool", "tool_name", "function"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .ok_or("falta el campo `name`")?
        .trim()
        .to_string();
    let args = ["arguments", "args", "parameters", "input", "params"]
        .iter()
        .find_map(|k| obj.get(*k).cloned())
        .unwrap_or_else(|| {
            // Sin campo de argumentos: el resto de claves son los argumentos.
            let mut m = Map::new();
            for (k, v) in &obj {
                if !["name", "tool", "tool_name"].contains(&k.as_str()) {
                    m.insert(k.clone(), v.clone());
                }
            }
            Value::Object(m)
        });
    let arguments = match args {
        Value::String(s) => lenient_json(&s).unwrap_or(Value::String(s)),
        Value::Null => Value::Object(Map::new()),
        v => v,
    };
    if !arguments.is_object() {
        return Err(format!("los argumentos de `{name}` deben ser un objeto JSON"));
    }
    Ok(ToolCall { id: String::new(), name, arguments })
}

fn parse_xml(b: &str) -> Result<ToolCall, String> {
    let tag = |t: &str| -> Option<String> {
        let o = format!("<{t}>");
        let c = format!("</{t}>");
        let s = b.find(&o)? + o.len();
        let e = b[s..].find(&c)? + s;
        Some(b[s..e].trim().to_string())
    };
    let name = tag("name").ok_or("XML sin <name>")?;
    let arguments = match tag("arguments") {
        Some(a) => lenient_json(&a).ok_or_else(|| format!("argumentos inválidos: {}", preview(&a)))?,
        None => Value::Object(Map::new()),
    };
    Ok(ToolCall { id: String::new(), name, arguments })
}

fn preview(s: &str) -> String {
    let p: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        format!("{p}…")
    } else {
        p
    }
}

/// Intenta parsear JSON aplicando reparaciones típicas de modelos pequeños.
pub fn lenient_json(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    let fixed = repair_json(s);
    serde_json::from_str(&fixed).ok()
}

fn repair_json(s: &str) -> String {
    // Comillas tipográficas.
    let s = s.replace(['“', '”'], "\"").replace(['‘', '’'], "'");
    let mut out = String::with_capacity(s.len() + 8);
    let mut in_str = false;
    let mut esc = false;
    let mut stack: Vec<char> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            match c {
                '\\' if !esc => {
                    esc = true;
                    out.push(c);
                }
                '"' if !esc => {
                    in_str = false;
                    out.push(c);
                }
                '\n' => {
                    out.push_str("\\n");
                    esc = false;
                }
                '\t' => {
                    out.push_str("\\t");
                    esc = false;
                }
                '\r' => esc = false,
                _ => {
                    esc = false;
                    out.push(c);
                }
            }
        } else {
            match c {
                '"' => {
                    in_str = true;
                    out.push(c);
                }
                '{' => {
                    stack.push('}');
                    out.push(c);
                }
                '[' => {
                    stack.push(']');
                    out.push(c);
                }
                '}' | ']' => {
                    // Quitar coma final antes del cierre.
                    let trimmed = out.trim_end().to_string();
                    if trimmed.ends_with(',') {
                        out = trimmed[..trimmed.len() - 1].to_string();
                    }
                    stack.pop();
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        i += 1;
    }
    if in_str {
        out.push('"');
    }
    let trimmed = out.trim_end().to_string();
    out = trimmed.strip_suffix(',').map(str::to_string).unwrap_or(trimmed);
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    out
}

/// Convierte el historial al formato de prompt-tools: las llamadas del asistente se
/// reescriben como bloques `<tool_call>` y los resultados como mensajes de usuario.
pub fn encode_history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|m| match m.role {
            Role::Assistant if !m.tool_calls.is_empty() => {
                let mut s = m.content.clone();
                for c in &m.tool_calls {
                    let v = serde_json::json!({"name": c.name, "arguments": c.arguments});
                    s.push_str(&format!("\n{OPEN}\n{v}\n{CLOSE}"));
                }
                Message::assistant(s.trim().to_string())
            }
            Role::Tool => Message::user(format!(
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                m.name.as_deref().unwrap_or("herramienta"),
                m.content
            )),
            _ => m.clone(),
        })
        .collect()
}

/// Filtro de streaming: deja pasar el texto pero oculta lo que va dentro de `<tool_call>`.
#[derive(Default)]
pub struct StreamFilter {
    pending: String,
    inside: bool,
}

impl StreamFilter {
    /// Devuelve el texto que se puede mostrar ya.
    pub fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut out = String::new();
        loop {
            if self.inside {
                match self.pending.find(CLOSE) {
                    Some(i) => {
                        self.pending.drain(..i + CLOSE.len());
                        self.inside = false;
                    }
                    None => return out,
                }
            } else {
                match self.pending.find(OPEN) {
                    Some(i) => {
                        out.push_str(&self.pending[..i]);
                        self.pending.drain(..i + OPEN.len());
                        self.inside = true;
                    }
                    None => {
                        // Retener un posible prefijo de "<tool_call>" al final.
                        let keep = (1..OPEN.len()).rev().find(|n| self.pending.ends_with(&OPEN[..*n])).unwrap_or(0);
                        let cut = self.pending.len() - keep;
                        out.push_str(&self.pending[..cut]);
                        self.pending.drain(..cut);
                        return out;
                    }
                }
            }
        }
    }

    pub fn finish(&mut self) -> String {
        if self.inside {
            self.pending.clear();
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn known() -> Vec<String> {
        vec!["read_file".into(), "grep".into(), "edit_file".into()]
    }

    #[test]
    fn standard_block() {
        let p = parse(
            "Voy a leerlo.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>",
            &known(),
        );
        assert_eq!(p.text, "Voy a leerlo.");
        assert_eq!(p.calls.len(), 1);
        assert_eq!(p.calls[0].name, "read_file");
        assert_eq!(p.calls[0].arguments, json!({"path": "a.rs"}));
        assert!(!p.calls[0].id.is_empty());
    }

    #[test]
    fn multiple_and_unclosed() {
        let p = parse(
            "<tool_call>{\"name\":\"grep\",\"arguments\":{\"pattern\":\"x\"}}</tool_call>\
             <tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"b\"}}",
            &known(),
        );
        assert_eq!(p.calls.len(), 2);
        assert_eq!(p.calls[1].arguments["path"], "b");
    }

    #[test]
    fn repairs_broken_json() {
        // coma final, comillas tipográficas, salto de línea crudo en string, llave sin cerrar
        let p = parse(
            "<tool_call>{“name”: “edit_file”, \"arguments\": {\"path\": \"a.py\", \"new_string\": \"def f():\n    pass\", \"old_string\": \"x\",}",
            &known(),
        );
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert_eq!(p.calls[0].arguments["new_string"], "def f():\n    pass");
    }

    #[test]
    fn alternative_keys_and_string_args() {
        let p =
            parse("<tool_call>{\"tool\": \"grep\", \"args\": \"{\\\"pattern\\\": \\\"y\\\"}\"}</tool_call>", &known());
        assert_eq!(p.calls[0].name, "grep");
        assert_eq!(p.calls[0].arguments["pattern"], "y");

        let p = parse(
            "<tool_call>{\"function\": {\"name\": \"grep\", \"parameters\": {\"pattern\": \"z\"}}}</tool_call>",
            &known(),
        );
        assert_eq!(p.calls[0].arguments["pattern"], "z");

        // sin campo de argumentos: el resto de claves
        let p = parse("<tool_call>{\"name\": \"read_file\", \"path\": \"c\"}</tool_call>", &known());
        assert_eq!(p.calls[0].arguments["path"], "c");
    }

    #[test]
    fn fenced_json_and_bare_json() {
        let p = parse("Uso:\n```json\n{\"name\": \"grep\", \"arguments\": {\"pattern\": \"q\"}}\n```", &known());
        assert_eq!(p.calls.len(), 1);
        assert_eq!(p.text, "Uso:");

        let p = parse("Llamo {\"name\": \"read_file\", \"arguments\": {\"path\": \"d\"}} ahora", &known());
        assert_eq!(p.calls[0].arguments["path"], "d");
        assert_eq!(p.text, "Llamo  ahora");
    }

    #[test]
    fn ignores_unrelated_json() {
        let txt = "Ejemplo de config:\n```json\n{\"name\": \"mi-paquete\", \"version\": \"1.0\"}\n```";
        let p = parse(txt, &known());
        assert!(p.calls.is_empty());
        assert!(p.text.contains("mi-paquete"));
    }

    #[test]
    fn xml_variant() {
        let p = parse("<tool_call><name>grep</name><arguments>{\"pattern\": \"w\"}</arguments></tool_call>", &known());
        assert_eq!(p.calls[0].name, "grep");
        assert_eq!(p.calls[0].arguments["pattern"], "w");
    }

    #[test]
    fn reports_errors() {
        let p = parse("<tool_call>esto no es json</tool_call>", &known());
        assert!(p.calls.is_empty());
        assert_eq!(p.errors.len(), 1);
    }

    #[test]
    fn stream_filter_hides_calls() {
        let mut f = StreamFilter::default();
        let mut shown = String::new();
        for chunk in ["Hola ", "<tool", "_call>{\"name\":", "\"x\"}</tool_", "call> fin", " <t"] {
            shown.push_str(&f.push(chunk));
        }
        shown.push_str(&f.finish());
        assert_eq!(shown, "Hola  fin <t");
    }

    #[test]
    fn history_encoding() {
        let call = ToolCall { id: "1".into(), name: "grep".into(), arguments: json!({"pattern": "a"}) };
        let h = encode_history(&[
            Message::assistant_with_tools("busco", vec![call.clone()]),
            Message::tool_result(&call, "res"),
        ]);
        assert!(h[0].tool_calls.is_empty());
        assert!(h[0].content.contains("<tool_call>"));
        assert_eq!(h[1].role, Role::User);
        assert!(h[1].content.contains("<tool_result name=\"grep\">"));
    }

    #[test]
    fn instructions_list_tools() {
        let spec = ToolSpec {
            name: "grep".into(),
            description: "busca".into(),
            parameters: json!({"type":"object","properties":{"pattern":{"type":"string"},"glob":{"type":"string"}},"required":["pattern"]}),
        };
        let s = instructions(&[spec]);
        assert!(s.contains("- grep(") && s.contains("pattern: string") && s.contains("glob?: string"));
    }
}
