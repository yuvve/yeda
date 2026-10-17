//! Reads and writes a yeda library on a local filesystem, for clients that use `std::fs`.

use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use yeda_core::Effect;
use yeda_core::library::{self, Library};

/// The file's contents, or `None` if it doesn't exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// Patterns a Syncthing-shared library should never sync: git's internals, which Syncthing and git
/// would corrupt between them, and the temporary files of atomic writes.
const STIGNORE: &str = "// Written by yeda.\n.git\n*.tmp\n";

/// Gives a Syncthing folder (one with `.stfolder`) a `.stignore`, unless it has one already.
fn ensure_stignore(dir: &Path) -> io::Result<()> {
    let ignore = dir.join(".stignore");
    if dir.join(".stfolder").exists() && !ignore.exists() {
        write_atomic(&ignore, STIGNORE.as_bytes())?;
    }
    Ok(())
}

/// Loads the library at `dir`, with `machine` as the per-device config overriding `yeda.toml`.
/// A Syncthing-shared library also gets a `.stignore` (see `STIGNORE`) if it has none.
pub fn load_library(dir: &Path, machine: Option<&Path>, home: Option<&Path>) -> Result<Library> {
    ensure_stignore(dir).with_context(|| dir.join(".stignore").display().to_string())?;
    let shared = read_optional(&dir.join(library::CONFIG))?;
    let machine = machine.map(read_optional).transpose()?.flatten();
    let mut refs = Vec::new();
    let refs_dir = dir.join(library::REFS);
    if refs_dir.exists() {
        for entry in fs::read_dir(&refs_dir).with_context(|| refs_dir.display().to_string())? {
            let path = entry?.path();
            if let Some(key) = path.file_name().and_then(|n| n.to_str()).and_then(library::ref_key) {
                refs.push((key.to_string(), fs::read_to_string(&path).with_context(|| path.display().to_string())?));
            }
        }
    }
    Library::parse(shared.as_deref(), machine.as_deref(), refs, home)
}

pub fn apply(dir: &Path, effects: impl IntoIterator<Item = Effect>) -> Result<()> {
    for effect in effects {
        match effect {
            Effect::Write { path, bytes } => write_atomic(&dir.join(&path), &bytes).with_context(|| path)?,
            Effect::Remove { path } => remove(&dir.join(&path)).with_context(|| path)?,
        }
    }
    Ok(())
}

/// Writes a temporary file and renames it into place, so a client reading concurrently never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

/// The server's version of a cached remote file, kept next to it.
fn version_path(cached: &Path) -> PathBuf {
    let mut p = cached.as_os_str().to_owned();
    p.push(".version");
    p.into()
}

pub fn set_cached_version(cached: &Path, version: Option<&str>) -> Result<()> {
    match version {
        Some(v) => write_atomic(&version_path(cached), v.as_bytes())?,
        None => remove(&version_path(cached))?,
    }
    Ok(())
}

/// Downloads a remote file to `cached` unless the copy there is current: `is_current` gets the
/// version stored with it and asks the server. Errors (offline, say) leave the cached copy alone.
pub fn sync_cached(
    cached: &Path,
    is_current: impl FnOnce(Option<&str>) -> Result<bool>,
    download: impl FnOnce() -> Result<(Vec<u8>, Option<String>)>,
) -> Result<()> {
    let version = read_optional(&version_path(cached))?;
    if cached.exists() && version.is_some() && is_current(version.as_deref())? {
        return Ok(());
    }
    let (bytes, version) = download()?;
    write_atomic(cached, &bytes)?;
    set_cached_version(cached, version.as_deref())
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_effects_and_loads() {
        let dir = std::env::temp_dir().join(format!("yeda-fs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let lib = Library::default();
        let r = library::Ref { key: "a2020".into(), title: "A".into(), ..Default::default() };
        apply(&dir, [lib.save(&r).unwrap(), Effect::write("annotations/a2020/img/x.png", "png")]).unwrap();
        assert_eq!(load_library(&dir, None, None).unwrap().refs, vec![r]);
        apply(&dir, lib.delete("a2020")).unwrap();
        assert!(load_library(&dir, None, None).unwrap().refs.is_empty());
        assert!(!dir.join("annotations/a2020").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
