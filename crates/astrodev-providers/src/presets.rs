//! Proveedores predefinidos para el asistente `/provider add`.

use astrodev_core::config::{ProviderConfig, ProviderKind};

#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    /// Variable de entorno estándar con la clave (alternativa al keyring).
    pub env_key: Option<&'static str>,
    pub needs_key: bool,
    pub default_model: Option<&'static str>,
    pub local: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        label: "Anthropic (Claude)",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.anthropic.com",
        env_key: Some("ANTHROPIC_API_KEY"),
        needs_key: true,
        default_model: Some("claude-opus-5-5"),
        local: false,
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        kind: ProviderKind::Openai,
        base_url: "https://api.openai.com/v1",
        env_key: Some("OPENAI_API_KEY"),
        needs_key: true,
        default_model: None,
        local: false,
    },
    Preset {
        id: "google",
        label: "Google Gemini",
        kind: ProviderKind::Google,
        base_url: "https://generativelanguage.googleapis.com",
        env_key: Some("GEMINI_API_KEY"),
        needs_key: true,
        default_model: None,
        local: false,
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        kind: ProviderKind::Openai,
        base_url: "https://openrouter.ai/api/v1",
        env_key: Some("OPENROUTER_API_KEY"),
        needs_key: true,
        default_model: None,
        local: false,
    },
    Preset {
        id: "groq",
        label: "Groq",
        kind: ProviderKind::Openai,
        base_url: "https://api.groq.com/openai/v1",
        env_key: Some("GROQ_API_KEY"),
        needs_key: true,
        default_model: None,
        local: false,
    },
    Preset {
        id: "deepseek",
        label: "DeepSeek",
        kind: ProviderKind::Openai,
        base_url: "https://api.deepseek.com/v1",
        env_key: Some("DEEPSEEK_API_KEY"),
        needs_key: true,
        default_model: Some("deepseek-chat"),
        local: false,
    },
    Preset {
        id: "mistral",
        label: "Mistral",
        kind: ProviderKind::Openai,
        base_url: "https://api.mistral.ai/v1",
        env_key: Some("MISTRAL_API_KEY"),
        needs_key: true,
        default_model: None,
        local: false,
    },
    Preset {
        id: "ollama",
        label: "Ollama (local)",
        kind: ProviderKind::Ollama,
        base_url: "http://localhost:11434",
        env_key: None,
        needs_key: false,
        default_model: None,
        local: true,
    },
    Preset {
        id: "lmstudio",
        label: "LM Studio (local)",
        kind: ProviderKind::Openai,
        base_url: "http://localhost:1234/v1",
        env_key: None,
        needs_key: false,
        default_model: None,
        local: true,
    },
    Preset {
        id: "llamacpp",
        label: "llama.cpp server (local)",
        kind: ProviderKind::Openai,
        base_url: "http://127.0.0.1:8080/v1",
        env_key: None,
        needs_key: false,
        default_model: None,
        local: true,
    },
    Preset {
        id: "vllm",
        label: "vLLM (local)",
        kind: ProviderKind::Openai,
        base_url: "http://localhost:8000/v1",
        env_key: None,
        needs_key: false,
        default_model: None,
        local: true,
    },
    Preset {
        id: "custom",
        label: "Otro endpoint compatible con OpenAI",
        kind: ProviderKind::Openai,
        base_url: "",
        env_key: None,
        needs_key: true,
        default_model: None,
        local: false,
    },
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Variable de entorno estándar para un proveedor configurado.
pub fn env_key_for(cfg: &ProviderConfig) -> Option<&'static str> {
    let url = cfg.base_url.as_deref().unwrap_or("");
    PRESETS
        .iter()
        .filter(|p| p.kind == cfg.kind && p.env_key.is_some())
        .find(|p| {
            url.is_empty() && p.kind != ProviderKind::Openai || (!p.base_url.is_empty() && url.starts_with(p.base_url))
        })
        .and_then(|p| p.env_key)
}

impl Preset {
    pub fn config(&self, base_url: &str, key_ref: Option<String>) -> ProviderConfig {
        ProviderConfig {
            kind: self.kind,
            base_url: Some(base_url.to_string()),
            api_key_ref: key_ref,
            default_model: self.default_model.map(str::to_string),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_lookup() {
        let groq = find("groq").unwrap().config("https://api.groq.com/openai/v1", None);
        assert_eq!(env_key_for(&groq), Some("GROQ_API_KEY"));
        let a = ProviderConfig { kind: ProviderKind::Anthropic, ..Default::default() };
        assert_eq!(env_key_for(&a), Some("ANTHROPIC_API_KEY"));
        let local = find("lmstudio").unwrap().config("http://localhost:1234/v1", None);
        assert_eq!(env_key_for(&local), None);
    }
}
