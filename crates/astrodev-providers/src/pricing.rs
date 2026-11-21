//! Coste por token. Precios en USD por millón de tokens.
//! Los modelos locales cuestan 0. Para otros, configura `[pricing."modelo"]`.

use astrodev_core::config::{Config, Price, ProviderKind};
use astrodev_core::provider::Usage;

/// Precios de la API de Anthropic (entrada, salida, entrada cacheada).
const ANTHROPIC: &[(&str, f64, f64, f64)] = &[
    ("claude-fable-5-2", 8.0, 40.0, 0.20),
    ("claude-fable-5-1", 10.0, 50.0, 1.0),
    ("claude-fable-5", 10.0, 50.0, 1.0),
    ("claude-opus-5-5", 4.0, 20.0, 0.20),
    ("claude-opus-5", 5.0, 25.0, 0.50),
    ("claude-opus-4-8", 5.0, 25.0, 0.50),
    ("claude-opus-4-7", 5.0, 25.0, 0.50),
    ("claude-opus-4-6", 5.0, 25.0, 0.50),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.10),
    ("claude-sonnet-5", 2.0, 10.0, 0.20),
    ("claude-sonnet-4-6", 3.0, 15.0, 0.30),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01),
    ("claude-haiku-4-5", 1.0, 5.0, 0.10),
];

pub fn builtin(model: &str) -> Option<Price> {
    ANTHROPIC.iter().find(|(id, ..)| *id == model).map(|(_, i, o, c)| Price {
        input: *i,
        output: *o,
        cached_input: Some(*c),
    })
}

pub fn cost(price: &Price, u: Usage) -> f64 {
    let cached = u.cached_input_tokens.min(u.input_tokens);
    let fresh = u.input_tokens - cached;
    let cached_price = price.cached_input.unwrap_or(price.input);
    (fresh as f64 * price.input + cached as f64 * cached_price + u.output_tokens as f64 * price.output) / 1e6
}

/// Coste de un uso con un proveedor/modelo según la configuración.
pub fn cost_for(cfg: &Config, provider: &str, model: &str, u: Usage) -> Option<f64> {
    let p = cfg.providers.get(provider)?;
    if p.kind == ProviderKind::Ollama || p.base_url.as_deref().is_some_and(crate::http_util::is_local) {
        return Some(0.0);
    }
    let price = cfg.pricing.get(model).copied().or_else(|| builtin(model))?;
    Some(cost(&price, u))
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrodev_core::config::ProviderConfig;

    #[test]
    fn computes_cost_with_cache() {
        let p = builtin("claude-opus-5-5").unwrap();
        let u = Usage { input_tokens: 1_000_000, output_tokens: 100_000, cached_input_tokens: 500_000 };
        // 0.5M × 4 + 0.5M × 0.2 + 0.1M × 20 = 2 + 0.1 + 2
        assert!((cost(&p, u) - 4.1).abs() < 1e-9);
    }

    #[test]
    fn local_is_free_and_config_overrides() {
        let mut cfg = Config::default();
        let u = Usage { input_tokens: 1000, output_tokens: 1000, ..Default::default() };
        assert_eq!(cost_for(&cfg, "ollama", "qwen", u), Some(0.0));
        cfg.providers.insert(
            "openai".into(),
            ProviderConfig {
                kind: ProviderKind::Openai,
                base_url: Some("https://api.openai.com/v1".into()),
                ..Default::default()
            },
        );
        assert_eq!(cost_for(&cfg, "openai", "gpt-x", u), None);
        cfg.pricing.insert("gpt-x".into(), Price { input: 1.0, output: 2.0, cached_input: None });
        assert!((cost_for(&cfg, "openai", "gpt-x", u).unwrap() - 0.003).abs() < 1e-12);
    }
}
