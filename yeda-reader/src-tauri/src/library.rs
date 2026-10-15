//! Reads a yeda library: `yeda.toml` and `refs/*.toml`.
// ponytail: a minimal reader until the shared yeda-core crate exists (Phase 5).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

/// Per-device settings: the library folder and folders for locations whose paths in
/// `yeda.toml` don't exist on this device.
#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    pub library: Option<PathBuf>,
    #[serde(default)]
    pub locations: BTreeMap<String, PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> Config {
        fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::annotations::write_atomic(path, &json).map_err(|e| e.to_string())
    }
}

#[derive(Serialize)]
pub struct RefSummary {
    key: String,
    title: String,
    authors: Vec<String>,
    year: Option<i64>,
    has_pdf: bool,
}

pub fn list_refs(library: &Path) -> Result<Vec<RefSummary>, String> {
    let dir = library.join("refs");
    let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut refs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let Ok(r) = read_table(&path) else { continue };
        let str_list = |k: &str| {
            r.get(k).and_then(Value::as_array).into_iter().flatten().filter_map(|v| v.as_str().map(Into::into)).collect()
        };
        refs.push(RefSummary {
            key: path.file_stem().unwrap().to_string_lossy().into(),
            title: r.get("title").and_then(Value::as_str).unwrap_or_default().into(),
            authors: str_list("authors"),
            year: r.get("year").and_then(Value::as_integer),
            has_pdf: r.contains_key("pdf"),
        });
    }
    refs.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(refs)
}

pub enum Resolved {
    Pdf(PathBuf),
    MissingLocation(String),
}

/// Finds the PDF of `key` as `locations[pdf.location] / pdf.path`, preferring this device's overrides.
pub fn resolve_pdf(library: &Path, key: &str, config: &Config) -> Result<Resolved, String> {
    let r = read_table(&library.join("refs").join(format!("{key}.toml")))?;
    let pdf = r.get("pdf").and_then(Value::as_table).ok_or_else(|| format!("{key} has no PDF"))?;
    let field = |k: &str| pdf.get(k).and_then(Value::as_str).ok_or_else(|| format!("{key}: pdf.{k} missing"));
    let (location, rel) = (field("location")?, field("path")?);

    let shared = read_table(&library.join("yeda.toml")).unwrap_or_default();
    let dir = config.locations.get(location).cloned().or_else(|| {
        shared.get("locations")?.get(location)?.as_str().map(expand_home)
    });
    match dir.map(|d| d.join(rel)) {
        Some(path) if path.is_file() => Ok(Resolved::Pdf(path)),
        _ => Ok(Resolved::MissingLocation(location.into())),
    }
}

fn read_table(path: &Path) -> Result<Table, String> {
    let s = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    s.parse().map_err(|e| format!("{}: {e}", path.display()))
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}
