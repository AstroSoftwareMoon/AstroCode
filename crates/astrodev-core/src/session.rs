//! Sesiones persistentes y reanudables (JSON en el directorio de datos).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checkpoint::Checkpoints;
use crate::message::{Message, Role};
use crate::provider::Usage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub project: PathBuf,
    pub provider: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub checkpoints: Checkpoints,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub project: PathBuf,
    pub model: String,
    pub updated_at: DateTime<Utc>,
    pub messages: usize,
}

pub fn new_id() -> String {
    let now = Utc::now();
    let rand = (now.timestamp_subsec_nanos() ^ std::process::id()) % 0xFFFF;
    format!("{}-{rand:04x}", now.format("%Y%m%d-%H%M%S"))
}

impl Session {
    pub fn new(project: &Path, provider: &str, model: &str) -> Self {
        let now = Utc::now();
        Self {
            id: new_id(),
            title: String::new(),
            project: project.to_path_buf(),
            provider: provider.into(),
            model: model.into(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            usage: Usage::default(),
            cost_usd: 0.0,
            checkpoints: Checkpoints::default(),
        }
    }

    /// Título a partir del primer mensaje del usuario.
    pub fn derive_title(&mut self) {
        if !self.title.is_empty() {
            return;
        }
        if let Some(m) = self.messages.iter().find(|m| m.role == Role::User) {
            let t: String = m.content.lines().next().unwrap_or("").chars().take(60).collect();
            self.title = t;
        }
    }
}

pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Almacén en el directorio de datos por defecto.
    pub fn default_store() -> Option<Self> {
        crate::config::data_dir().ok().map(|d| Self::new(d.join("sessions")))
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn save(&self, s: &Session) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{}.tmp", s.id));
        std::fs::write(&tmp, serde_json::to_vec(s)?)?;
        std::fs::rename(tmp, self.path(&s.id))
    }

    pub fn load(&self, id: &str) -> std::io::Result<Session> {
        // Permite prefijos únicos del id.
        let path = if self.path(id).exists() {
            self.path(id)
        } else {
            let matches: Vec<SessionMeta> = self.list(None).into_iter().filter(|m| m.id.starts_with(id)).collect();
            match matches.as_slice() {
                [one] => self.path(&one.id),
                [] => {
                    return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no existe la sesión {id}")))
                }
                _ => return Err(std::io::Error::other(format!("el prefijo {id} es ambiguo"))),
            }
        };
        let data = std::fs::read(path)?;
        serde_json::from_slice(&data).map_err(std::io::Error::other)
    }

    /// Sesiones (opcionalmente de un proyecto), más recientes primero.
    pub fn list(&self, project: Option<&Path>) -> Vec<SessionMeta> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<SessionMeta> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|d| serde_json::from_slice::<Session>(&d).ok())
            .filter(|s| project.is_none_or(|p| s.project == p))
            .map(|s| SessionMeta {
                messages: s.messages.len(),
                id: s.id,
                title: s.title,
                project: s.project,
                model: s.model,
                updated_at: s.updated_at,
            })
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        out
    }

    pub fn latest(&self, project: &Path) -> Option<Session> {
        let m = self.list(Some(project)).into_iter().next()?;
        self.load(&m.id).ok()
    }

    pub fn delete(&self, id: &str) -> std::io::Result<()> {
        std::fs::remove_file(self.path(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_load() {
        let d = tempfile::tempdir().unwrap();
        let store = SessionStore::new(d.path());
        let mut s1 = Session::new(Path::new("/p1"), "ollama", "m");
        s1.messages.push(Message::user("Arregla el bug del login\nmás texto"));
        s1.derive_title();
        store.save(&s1).unwrap();
        let mut s2 = Session::new(Path::new("/p2"), "ollama", "m");
        s2.id = "otro-id".into();
        store.save(&s2).unwrap();

        assert_eq!(s1.title, "Arregla el bug del login");
        assert_eq!(store.list(None).len(), 2);
        let l = store.list(Some(Path::new("/p1")));
        assert_eq!(l.len(), 1);
        let back = store.load(&s1.id).unwrap();
        assert_eq!(back.messages, s1.messages);
        assert_eq!(store.load("otro").unwrap().id, "otro-id");
        assert_eq!(store.latest(Path::new("/p1")).unwrap().id, s1.id);
        assert!(store.load("nada").is_err());
    }
}
