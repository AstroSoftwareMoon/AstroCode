//! Claves de API: keyring del sistema (Keychain, Credential Manager, Secret Service).
//! Nunca se guardan en texto plano. Alternativa sin keyring: variables de entorno.

use tokio::sync::OnceCell;

pub const SERVICE: &str = "astrodev";

/// Variable de entorno específica: `ASTRODEV_KEY_<REF>` (en mayúsculas, `-`→`_`).
pub fn env_var_for(reference: &str) -> String {
    format!("ASTRODEV_KEY_{}", reference.to_uppercase().replace(['-', '.', ' '], "_"))
}

fn entry(reference: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, reference).map_err(|e| e.to_string())
}

/// Lee la clave del keyring (bloqueante: llamar desde `spawn_blocking`).
pub fn keyring_get(reference: &str) -> Option<String> {
    entry(reference).ok()?.get_password().ok().filter(|k| !k.is_empty())
}

pub fn keyring_set(reference: &str, key: &str) -> Result<(), String> {
    entry(reference)?.set_password(key).map_err(|e| {
        format!(
            "no se pudo guardar en el keyring del sistema ({e}). Alternativa: exporta {}=<clave>",
            env_var_for(reference)
        )
    })
}

pub fn keyring_delete(reference: &str) -> Result<(), String> {
    match entry(reference)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Fuente de una clave: se resuelve la primera vez que se necesita.
pub struct KeySource {
    reference: Option<String>,
    env_vars: Vec<String>,
    cached: OnceCell<Option<String>>,
}

impl KeySource {
    pub fn new(reference: Option<String>, preset_env: Option<&str>) -> Self {
        let mut env_vars = Vec::new();
        if let Some(r) = &reference {
            env_vars.push(env_var_for(r));
        }
        if let Some(e) = preset_env {
            env_vars.push(e.to_string());
        }
        Self { reference, env_vars, cached: OnceCell::new() }
    }

    /// Clave fija (tests).
    pub fn fixed(key: Option<String>) -> Self {
        let cell = OnceCell::new();
        let _ = cell.set(key);
        Self { reference: None, env_vars: Vec::new(), cached: cell }
    }

    pub async fn get(&self) -> Option<String> {
        self.cached
            .get_or_init(|| async {
                for v in &self.env_vars {
                    if let Ok(k) = std::env::var(v) {
                        if !k.trim().is_empty() {
                            return Some(k.trim().to_string());
                        }
                    }
                }
                let r = self.reference.clone()?;
                tokio::task::spawn_blocking(move || keyring_get(&r)).await.ok().flatten()
            })
            .await
            .clone()
    }

    pub fn describe(&self) -> String {
        match &self.reference {
            Some(r) => format!("keyring `{SERVICE}/{r}` o la variable {}", self.env_vars.join(" / ")),
            None => self.env_vars.join(" / "),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names() {
        assert_eq!(env_var_for("open-router"), "ASTRODEV_KEY_OPEN_ROUTER");
    }

    #[tokio::test]
    async fn env_var_wins_over_keyring() {
        std::env::set_var("ASTRODEV_KEY_TEST_PROV", "sk-env");
        let k = KeySource::new(Some("test-prov".into()), None);
        assert_eq!(k.get().await.as_deref(), Some("sk-env"));
        std::env::remove_var("ASTRODEV_KEY_TEST_PROV");
    }

    #[tokio::test]
    async fn missing_key_is_none() {
        let k = KeySource::new(Some("no-existe-zzz".into()), Some("ASTRODEV_NO_EXISTE"));
        assert_eq!(k.get().await, None);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod keyring_runtime {
    /// El keyring se usa desde `spawn_blocking`: no debe hacer pánico aunque no haya
    /// servicio de secretos (servidores sin escritorio, CI).
    #[tokio::test(flavor = "multi_thread")]
    async fn keyring_from_blocking_thread_does_not_panic() {
        let r = tokio::task::spawn_blocking(|| super::keyring_set("astrodev-test-entry", "x")).await;
        assert!(r.is_ok(), "pánico en el keyring");
        let _ = tokio::task::spawn_blocking(|| super::keyring_delete("astrodev-test-entry")).await;
    }
}
