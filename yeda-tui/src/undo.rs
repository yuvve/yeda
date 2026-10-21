//! Undo and redo of what commands wrote to the library, for this session.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use yeda_core::Effect;

const LIMIT: usize = 100;

/// The command that writes are recorded under: one per key press, kept by background jobs it starts.
#[derive(Clone, Default)]
pub struct Step {
    pub id: u64,
    pub label: String,
}

/// A library file before and after a write; `None` is no file.
struct Change {
    path: String,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

struct Entry {
    step: Step,
    changes: Vec<Change>,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl History {
    /// Applies `effects` to the library at `dir`, remembering what they replace under `step`.
    pub fn apply(&mut self, dir: &Path, step: &Step, effects: impl IntoIterator<Item = Effect>) -> Result<()> {
        let effects: Vec<Effect> = effects.into_iter().collect();
        let mut changes = Vec::new();
        for effect in &effects {
            match effect {
                Effect::Write { path, bytes } => changes.push(Change { path: path.clone(), before: read(dir, path)?, after: Some(bytes.clone()) }),
                Effect::Remove { path } => {
                    for file in files_under(dir, path)? {
                        changes.push(Change { before: read(dir, &file)?, path: file, after: None });
                    }
                }
            }
        }
        yeda_fs::apply(dir, effects)?;
        if changes.is_empty() {
            return Ok(());
        }
        self.redo.clear();
        // A background job's results join the command that started it.
        match self.undo.iter_mut().rev().find(|e| e.step.id == step.id) {
            Some(entry) => entry.changes.extend(changes),
            None => self.undo.push(Entry { step: step.clone(), changes }),
        }
        let excess = self.undo.len().saturating_sub(LIMIT);
        self.undo.drain(..excess);
        Ok(())
    }

    /// Takes back the last command's writes; returns its label and how many files it restored.
    pub fn undo(&mut self, dir: &Path) -> Result<(String, usize)> {
        let entry = self.undo.pop().context("nothing to undo")?;
        match restore(dir, &entry, true) {
            Ok(n) => {
                let label = entry.step.label.clone();
                self.redo.push(entry);
                Ok((label, n))
            }
            Err(e) => {
                self.undo.push(entry);
                Err(e)
            }
        }
    }

    pub fn redo(&mut self, dir: &Path) -> Result<(String, usize)> {
        let entry = self.redo.pop().context("nothing to redo")?;
        match restore(dir, &entry, false) {
            Ok(n) => {
                let label = entry.step.label.clone();
                self.undo.push(entry);
                Ok((label, n))
            }
            Err(e) => {
                self.redo.push(entry);
                Err(e)
            }
        }
    }
}

/// Puts each file of `entry` back as it was before (`back`) or after it, once every one of them
/// is still as the entry left it; a file changed since (by a sync, the reader) stops it.
fn restore(dir: &Path, entry: &Entry, back: bool) -> Result<usize> {
    let mut expected: HashMap<&str, &Option<Vec<u8>>> = HashMap::new();
    let mut wanted: HashMap<&str, &Option<Vec<u8>>> = HashMap::new();
    let ordered: Box<dyn Iterator<Item = &Change>> = if back { Box::new(entry.changes.iter()) } else { Box::new(entry.changes.iter().rev()) };
    for c in ordered {
        let (now, then) = if back { (&c.after, &c.before) } else { (&c.before, &c.after) };
        // The last write to a file is what it holds now; the first is what it held before.
        expected.insert(&c.path, now);
        wanted.entry(&c.path).or_insert(then);
    }
    for (path, content) in &expected {
        if read(dir, path)? != **content {
            bail!("{path} changed since; nothing was {}", if back { "undone" } else { "redone" });
        }
    }
    let effects = wanted.iter().map(|(path, content)| match content {
        Some(bytes) => Effect::Write { path: path.to_string(), bytes: bytes.clone() },
        None => Effect::remove(path.to_string()),
    });
    yeda_fs::apply(dir, effects.collect::<Vec<_>>())?;
    Ok(wanted.len())
}

fn read(dir: &Path, path: &str) -> Result<Option<Vec<u8>>> {
    match fs::read(dir.join(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => Ok(None),
        Err(e) => Err(e).with_context(|| path.to_string()),
    }
}

/// `path` if it is a file, else every file under it, relative to `dir`.
fn files_under(dir: &Path, path: &str) -> Result<Vec<String>> {
    let full = dir.join(path);
    if !full.is_dir() {
        return Ok(if full.exists() { vec![path.to_string()] } else { vec![] });
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&full).with_context(|| path.to_string())? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        files.extend(files_under(dir, &format!("{path}/{name}"))?);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undoes_and_redoes_a_step_unless_a_file_changed() {
        let dir = std::env::temp_dir().join(format!("yeda-undo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("annotations/a/1")).unwrap();
        fs::write(dir.join("annotations/a/1/annotations.toml"), "old").unwrap();
        let mut h = History::default();
        let step = Step { id: 1, label: "delete".into() };
        h.apply(&dir, &step, [Effect::write("refs/b.toml", "new"), Effect::remove("annotations/a")]).unwrap();
        h.apply(&dir, &step, [Effect::write("refs/b.toml", "newer")]).unwrap();
        assert!(!dir.join("annotations/a").exists());

        assert_eq!(h.undo(&dir).unwrap(), ("delete".to_string(), 2));
        assert!(!dir.join("refs/b.toml").exists());
        assert_eq!(fs::read_to_string(dir.join("annotations/a/1/annotations.toml")).unwrap(), "old");

        h.redo(&dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("refs/b.toml")).unwrap(), "newer");
        fs::write(dir.join("refs/b.toml"), "synced").unwrap();
        assert!(h.undo(&dir).is_err());
        assert_eq!(fs::read_to_string(dir.join("refs/b.toml")).unwrap(), "synced");
        fs::remove_dir_all(&dir).unwrap();
    }
}
