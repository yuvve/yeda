//! The `:discover` cache: search results and OpenAlex lookups, per device and outside the library.

use anyhow::{Context, Result};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn path(cache: Option<&Path>, kind: &str, key: &str) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    Some(cache?.join("discover").join(kind).join(format!("{:016x}", hasher.finish())))
}

pub fn get(cache: Option<&Path>, kind: &str, key: &str) -> Option<String> {
    // shortcut: entries never expire; :discover! bypasses them.
    fs::read_to_string(path(cache, kind, key)?).ok()
}

/// Best effort: a cache that cannot be written only costs another request.
pub fn put(cache: Option<&Path>, kind: &str, key: &str, body: &str) {
    if let Some(path) = path(cache, kind, key) {
        let _ = crate::write_atomic(&path, body.as_bytes());
    }
}

/// Runs `f` no sooner than `gap` after the last call through `dir`, one caller at a time across
/// processes; the directory is the lock and holds the time of the last call.
pub fn spaced<T>(dir: Option<&Path>, gap: Duration, f: impl FnOnce() -> T) -> Result<T> {
    let Some(dir) = dir else { return Ok(f()) };
    crate::locked(dir, || {
        let last = fs::read_to_string(dir.join("last")).ok().and_then(|t| t.trim().parse::<u128>().ok());
        if let Some(last) = last {
            let elapsed = millis().saturating_sub(last);
            if elapsed < gap.as_millis() {
                std::thread::sleep(gap - Duration::from_millis(elapsed as u64));
            }
        }
        let out = f();
        crate::write_atomic(&dir.join("last"), millis().to_string().as_bytes()).context("writing the arXiv request time")?;
        Ok(out)
    })
}

fn millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_and_returns_cache_entries() {
        let dir = std::env::temp_dir().join(format!("yeda-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(get(Some(&dir), "search", "exa|q"), None);
        put(Some(&dir), "search", "exa|q", "[{\"title\":\"A\"}]");
        assert_eq!(get(Some(&dir), "search", "exa|q").as_deref(), Some("[{\"title\":\"A\"}]"));
        assert_eq!(get(Some(&dir), "search", "exa|other"), None);
        put(None, "search", "exa|q", "ignored");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn spaces_calls_across_processes() {
        let dir = std::env::temp_dir().join(format!("yeda-spaced-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let started = std::time::Instant::now();
        spaced(Some(&dir), Duration::from_millis(120), || ()).unwrap();
        spaced(Some(&dir), Duration::from_millis(120), || ()).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(120));
        let _ = fs::remove_dir_all(&dir);
    }
}
