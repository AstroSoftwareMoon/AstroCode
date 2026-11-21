//! Checkpoints para `/undo`: antes de que una herramienta modifique un archivo se guarda
//! su contenido original. Cada turno del usuario abre un checkpoint nuevo; deshacer
//! restaura los archivos y recorta la conversación al punto anterior al turno.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: PathBuf,
    /// `None` si el archivo no existía (deshacer lo borra).
    pub original: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Nº de mensajes de la conversación antes del turno.
    pub message_index: usize,
    /// Texto del usuario que abrió el turno.
    pub label: String,
    pub files: Vec<FileSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checkpoints {
    stack: Vec<Checkpoint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UndoReport {
    pub label: String,
    pub message_index: usize,
    pub restored: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

const MAX_CHECKPOINTS: usize = 50;

impl Checkpoints {
    pub fn begin(&mut self, message_index: usize, label: impl Into<String>) {
        self.stack.push(Checkpoint {
            message_index,
            label: label.into(),
            files: Vec::new(),
        });
        if self.stack.len() > MAX_CHECKPOINTS {
            self.stack.remove(0);
        }
    }

    /// Guarda el estado actual de `path` si aún no se guardó en este checkpoint.
    pub fn record(&mut self, path: &Path) {
        let Some(cp) = self.stack.last_mut() else {
            return;
        };
        if cp.files.iter().any(|f| f.path == path) {
            return;
        }
        let original = if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(s) => Some(s),
                Err(_) => return, // binario/ilegible: no lo podemos restaurar
            }
        } else {
            None
        };
        cp.files.push(FileSnapshot {
            path: path.to_path_buf(),
            original,
        });
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn last(&self) -> Option<&Checkpoint> {
        self.stack.last()
    }

    /// Deshace el último checkpoint restaurando archivos.
    pub fn undo(&mut self) -> Option<UndoReport> {
        let cp = self.stack.pop()?;
        let mut rep = UndoReport {
            label: cp.label,
            message_index: cp.message_index,
            restored: Vec::new(),
            deleted: Vec::new(),
            failed: Vec::new(),
        };
        for f in cp.files.into_iter().rev() {
            let res = match &f.original {
                Some(content) => {
                    if let Some(parent) = f.path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&f.path, content).map(|_| rep.restored.push(f.path.clone()))
                }
                None => match std::fs::remove_file(&f.path) {
                    Ok(()) => {
                        rep.deleted.push(f.path.clone());
                        Ok(())
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                },
            };
            if let Err(e) = res {
                rep.failed.push((f.path, e.to_string()));
            }
        }
        Some(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_restores_and_deletes() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.txt");
        let b = d.path().join("b.txt");
        std::fs::write(&a, "original").unwrap();

        let mut cps = Checkpoints::default();
        cps.begin(0, "turno 1");
        cps.record(&a);
        std::fs::write(&a, "cambio 1").unwrap();
        cps.record(&a); // segunda vez: se ignora, se conserva el original
        std::fs::write(&a, "cambio 2").unwrap();
        cps.record(&b);
        std::fs::write(&b, "nuevo").unwrap();

        let rep = cps.undo().unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "original");
        assert!(!b.exists());
        assert_eq!(rep.restored, vec![a]);
        assert_eq!(rep.deleted, vec![b]);
        assert!(cps.undo().is_none());
    }

    #[test]
    fn nested_turns_undo_in_order() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, "v0").unwrap();
        let mut cps = Checkpoints::default();
        cps.begin(0, "t1");
        cps.record(&a);
        std::fs::write(&a, "v1").unwrap();
        cps.begin(4, "t2");
        cps.record(&a);
        std::fs::write(&a, "v2").unwrap();
        assert_eq!(cps.undo().unwrap().message_index, 4);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v1");
        cps.undo();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v0");
    }
}
