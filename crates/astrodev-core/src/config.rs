//! Configuración en TOML (`~/.config/astrodev/config.toml`).
//!
//! Las claves de API nunca se guardan aquí: sólo una referencia al keyring del sistema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "config.toml";
/// Variable de entorno para usar otro directorio de configuración (tests, portable).
pub const CONFIG_DIR_ENV: &str = "ASTRODEV_CONFIG_DIR";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    Ollama,
    /// Cualquier endpoint compatible con la API de OpenAI (llama.cpp, LM Studio, vLLM…).
    Openai,
    Anthropic,
    Google,
}

/// Cómo se hace el tool calling con este proveedor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToolMode {
    /// Nativo si el modelo lo admite; si no, por prompt.
    #[default]
    Auto,
    Native,
    /// Formato JSON/XML en el texto, parseado por AstroDev (modelos pequeños).
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Nombre de la entrada en el keyring (servicio `astrodev`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
    #[serde(default)]
    pub tool_mode: ToolMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Pregunta antes de cualquier herramienta con efectos.
    #[default]
    Ask,
    /// Automático para operaciones dentro del proyecto.
    AutoProject,
    /// Automático para todo (salvo comandos peligrosos, que siempre preguntan).
    AutoAll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub mode: PermissionMode,
    /// Herramientas siempre permitidas sin preguntar (p.ej. `read_file`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_allow: Vec<String>,
    /// Patrones extra de comandos considerados peligrosos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dangerous_patterns: Vec<String>,
}

/// Ajustes por modelo (los rellena el test de humo del instalador o el usuario).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Capas en GPU (llama.cpp `-ngl`, Ollama `num_gpu`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_layers: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_mode: Option<ToolMode>,
    /// Velocidad medida en el test de humo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_tps: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Máximo de iteraciones herramienta→modelo por turno.
    pub max_steps: u32,
    /// Fracción del contexto a partir de la cual se resume la conversación.
    pub summarize_at: f64,
    /// Contexto por defecto si el modelo no lo informa.
    pub default_context_window: u32,
    pub temperature: Option<f64>,
    /// Reintentos cuando el modelo produce una llamada mal formada (modo prompt).
    #[serde(default = "default_retries")]
    pub tool_parse_retries: u32,
}

fn default_retries() -> u32 {
    2
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: 25,
            summarize_at: 0.8,
            default_context_window: 8192,
            temperature: Some(0.2),
            tool_parse_retries: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub default_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    /// Ajustes por modelo: `[models."qwen2.5-coder:7b"]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelSettings>,
    /// URL del catálogo de modelos (JSON) para el recomendador.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = BTreeMap::new();
        providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                kind: ProviderKind::Ollama,
                base_url: Some("http://localhost:11434".into()),
                ..Default::default()
            },
        );
        Self {
            default_provider: "ollama".into(),
            default_model: None,
            providers,
            permissions: PermissionsConfig::default(),
            agent: AgentConfig::default(),
            models: BTreeMap::new(),
            catalog_url: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no se pudo determinar el directorio de configuración")]
    NoConfigDir,
    #[error("error leyendo {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML inválido en {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("error serializando config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "astrodev", "astrodev")
}

/// Directorio de configuración. En Linux `~/.config/astrodev`.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    // En macOS preferimos también ~/.config/astrodev si existe (convención de CLIs).
    if cfg!(target_os = "macos") {
        if let Some(home) = directories::BaseDirs::new() {
            let xdg = home.home_dir().join(".config/astrodev");
            if xdg.exists() {
                return Ok(xdg);
            }
        }
    }
    project_dirs().map(|d| d.config_dir().to_path_buf()).ok_or(ConfigError::NoConfigDir)
}

/// Directorio de datos (sesiones, logs, modelos descargados).
pub fn data_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV) {
        return Ok(PathBuf::from(dir).join("data"));
    }
    project_dirs().map(|d| d.data_dir().to_path_buf()).ok_or(ConfigError::NoConfigDir)
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(config_dir()?.join(CONFIG_FILE))
    }

    pub fn from_toml(s: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|source| ConfigError::Parse { path: path.to_path_buf(), source })
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let s = std::fs::read_to_string(path).map_err(|source| ConfigError::Io { path: path.to_path_buf(), source })?;
        Self::from_toml(&s, path)
    }

    /// Carga la config; si no existe devuelve la de por defecto y `true` (primera ejecución).
    pub fn load_or_default(path: &Path) -> Result<(Self, bool), ConfigError> {
        if path.exists() {
            Ok((Self::load(path)?, false))
        } else {
            Ok((Self::default(), true))
        }
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io { path: parent.to_path_buf(), source })?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s).map_err(|source| ConfigError::Io { path: path.to_path_buf(), source })
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let cfg = Config::default();
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn parses_user_toml() {
        let s = r#"
            default_provider = "local"
            default_model = "qwen2.5-coder:7b"

            [providers.local]
            kind = "ollama"
            base_url = "http://127.0.0.1:11434"
            tool_mode = "prompt"

            [providers.groq]
            kind = "openai"
            base_url = "https://api.groq.com/openai/v1"
            api_key_ref = "groq"

            [permissions]
            mode = "auto-project"
            always_allow = ["read_file", "grep"]
        "#;
        let cfg = Config::from_toml(s, Path::new("x")).unwrap();
        assert_eq!(cfg.default_provider, "local");
        assert_eq!(cfg.providers["local"].tool_mode, ToolMode::Prompt);
        assert_eq!(cfg.providers["groq"].kind, ProviderKind::Openai);
        assert_eq!(cfg.permissions.mode, PermissionMode::AutoProject);
        assert_eq!(cfg.agent.max_steps, 25);
    }

    #[test]
    fn invalid_toml_reports_path() {
        let err = Config::from_toml("default_provider = ", Path::new("/x/config.toml")).unwrap_err().to_string();
        assert!(err.contains("/x/config.toml"));
    }
}
