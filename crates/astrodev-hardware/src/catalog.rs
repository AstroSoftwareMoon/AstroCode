//! Catálogo de modelos (JSON). Copia embebida + actualización desde GitHub.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const EMBEDDED: &str = include_str!("../../../catalog/models.json");
pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/AstroSoftwareMoon/AstroCode/main/catalog/models.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GgufSource {
    /// Repositorio de Hugging Face (`usuario/repo`).
    pub repo: String,
    pub file: String,
}

impl GgufSource {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/main/{}", self.repo, self.file)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantVariant {
    pub name: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gguf: Option<GgufSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl QuantVariant {
    /// Etiqueta para `ollama pull`. Si no hay etiqueta oficial se usa el GGUF de
    /// Hugging Face (`hf.co/usuario/repo:QUANT`), que Ollama también sabe descargar.
    pub fn ollama_pull_name(&self) -> Option<String> {
        self.ollama_tag.clone().or_else(|| self.gguf.as_ref().map(|g| format!("hf.co/{}:{}", g.repo, self.name)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub family: String,
    pub params_b: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_params_b: Option<f64>,
    pub layers: u32,
    pub kv_heads: u32,
    pub head_dim: u32,
    pub context_max: u32,
    pub supports_tools: bool,
    pub code_score: u32,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    pub quants: Vec<QuantVariant>,
}

impl CatalogModel {
    pub fn quant(&self, name: &str) -> Option<&QuantVariant> {
        self.quants.iter().find(|q| q.name.eq_ignore_ascii_case(name))
    }

    /// Fracción de pesos que se leen por token (MoE < 1).
    pub fn active_fraction(&self) -> f64 {
        self.active_params_b.map(|a| (a / self.params_b).clamp(0.05, 1.0)).unwrap_or(1.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub notes: String,
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    pub fn parse(s: &str) -> Result<Self, String> {
        let c: Catalog = serde_json::from_str(s).map_err(|e| format!("catálogo inválido: {e}"))?;
        if c.models.is_empty() {
            return Err("el catálogo no tiene modelos".into());
        }
        Ok(c)
    }

    pub fn embedded() -> Self {
        Self::parse(EMBEDDED).expect("catálogo embebido válido")
    }

    pub fn cache_path(data_dir: &Path) -> PathBuf {
        data_dir.join("catalog.json")
    }

    /// Carga la copia en caché si es más nueva que la embebida.
    pub fn load(data_dir: &Path) -> Self {
        let embedded = Self::embedded();
        match std::fs::read_to_string(Self::cache_path(data_dir)).ok().and_then(|s| Self::parse(&s).ok()) {
            Some(c) if c.version >= embedded.version => c,
            _ => embedded,
        }
    }

    /// ¿La caché tiene más de un día?
    pub fn cache_is_stale(data_dir: &Path) -> bool {
        std::fs::metadata(Self::cache_path(data_dir))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age.as_secs() > 86_400)
    }

    /// Descarga el catálogo de `url`, lo valida y lo guarda en caché.
    pub async fn update(url: &str, data_dir: &Path) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(url).send().await.map_err(|e| format!("no se pudo descargar el catálogo: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("el servidor respondió {}", resp.status()));
        }
        let body = resp.text().await.map_err(|e| e.to_string())?;
        let c = Self::parse(&body)?;
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        std::fs::write(Self::cache_path(data_dir), &body).map_err(|e| e.to_string())?;
        Ok(c)
    }

    pub fn find(&self, id: &str) -> Option<&CatalogModel> {
        let id_l = id.to_lowercase();
        self.models.iter().find(|m| m.id == id_l).or_else(|| {
            // También por etiqueta de Ollama.
            self.models.iter().find(|m| m.quants.iter().any(|q| q.ollama_tag.as_deref() == Some(id)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn embedded_is_valid() {
        let c = Catalog::embedded();
        assert!(c.models.len() >= 10);
        for m in &c.models {
            assert!(!m.quants.is_empty(), "{}", m.id);
            assert!(m.code_score <= 100);
            assert!(m.layers > 0 && m.kv_heads > 0 && m.head_dim > 0, "{}", m.id);
        }
        let q = c.find("qwen2.5-coder-7b").unwrap();
        assert_eq!(q.quant("q4_k_m").unwrap().ollama_tag.as_deref(), Some("qwen2.5-coder:7b"));
        assert_eq!(c.find("qwen2.5-coder:7b").unwrap().id, "qwen2.5-coder-7b");
    }

    #[test]
    fn hf_pull_name_fallback() {
        let c = Catalog::embedded();
        let m = c.find("qwen2.5-coder-7b").unwrap();
        let q5 = m.quant("Q5_K_M").unwrap();
        assert_eq!(q5.ollama_pull_name().unwrap(), "hf.co/bartowski/Qwen2.5-Coder-7B-Instruct-GGUF:Q5_K_M");
        assert!(q5.gguf.as_ref().unwrap().url().starts_with("https://huggingface.co/bartowski/"));
    }

    #[tokio::test]
    async fn update_and_cache() {
        let server = MockServer::start().await;
        let mut c = Catalog::embedded();
        c.version = 999;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(serde_json::to_string(&c).unwrap()))
            .mount(&server)
            .await;
        let d = tempfile::tempdir().unwrap();
        assert!(Catalog::cache_is_stale(d.path()));
        let got = Catalog::update(&server.uri(), d.path()).await.unwrap();
        assert_eq!(got.version, 999);
        assert_eq!(Catalog::load(d.path()).version, 999);
        assert!(!Catalog::cache_is_stale(d.path()));
    }

    #[tokio::test]
    async fn invalid_update_keeps_cache() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string("{}")).mount(&server).await;
        let d = tempfile::tempdir().unwrap();
        assert!(Catalog::update(&server.uri(), d.path()).await.is_err());
        assert_eq!(Catalog::load(d.path()).version, Catalog::embedded().version);
    }
}
