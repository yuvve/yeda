use crate::llm::Profile;
use crate::tables::Table;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pdf {
    pub location: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ref {
    #[serde(skip)]
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    pub tags: Vec<String>,
    pub collections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf: Option<Pdf>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
    /// This reference's row in each table, by table name then column.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tables: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Config {
    locations: BTreeMap<String, String>,
    tag_colors: BTreeMap<String, String>,
    searches: BTreeMap<String, String>,
    reader: Option<String>,
    tables: BTreeMap<String, Table>,
    llm: BTreeMap<String, Profile>,
    default_llm: Option<String>,
}

pub const STATUSES: [&str; 5] = ["New", "To Read", "In Progress", "Read", "Not Reading"];

pub struct Library {
    pub dir: PathBuf,
    pub locations: BTreeMap<String, PathBuf>,
    pub tag_colors: BTreeMap<String, String>,
    pub searches: BTreeMap<String, String>,
    /// Command that opens PDFs as `<reader> <pdf> --annotations <dir>`; the system viewer when unset.
    pub reader: Option<String>,
    pub tables: BTreeMap<String, Table>,
    pub llm: BTreeMap<String, Profile>,
    pub default_llm: Option<String>,
    pub refs: Vec<Ref>,
}

impl Library {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut locations = BTreeMap::new();
        let mut tag_colors = BTreeMap::new();
        let mut searches = BTreeMap::new();
        let mut reader = None;
        let mut tables = BTreeMap::new();
        let mut llm = BTreeMap::new();
        let mut default_llm = None;
        // Machine-local config is read last so it overrides the library's shared defaults.
        let machine = dirs::config_dir().map(|d| d.join("yeda/config.toml"));
        for path in [Some(dir.join("yeda.toml")), machine].into_iter().flatten() {
            if path.exists() {
                let cfg: Config = toml::from_str(&fs::read_to_string(&path)?)
                    .with_context(|| path.display().to_string())?;
                for (name, p) in cfg.locations {
                    locations.insert(name, expand_home(&p));
                }
                tag_colors.extend(cfg.tag_colors);
                searches.extend(cfg.searches);
                reader = cfg.reader.or(reader);
                tables.extend(cfg.tables);
                llm.extend(cfg.llm);
                default_llm = cfg.default_llm.or(default_llm);
            }
        }

        let mut refs = Vec::new();
        let refs_dir = dir.join("refs");
        if refs_dir.exists() {
            for entry in fs::read_dir(&refs_dir)? {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "toml") {
                    let mut r: Ref = toml::from_str(&fs::read_to_string(&path)?)
                        .with_context(|| path.display().to_string())?;
                    r.key = path.file_stem().unwrap().to_string_lossy().into();
                    refs.push(r);
                }
            }
        }
        refs.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Self { dir: dir.into(), locations, tag_colors, searches, reader, tables, llm, default_llm, refs })
    }

    /// Edits the shared `yeda.toml` (never the machine-local config).
    // ponytail: rewrites the file through toml::Table, dropping comments; use toml_edit if that matters.
    pub fn update_config(&self, edit: impl FnOnce(&mut toml::Table) -> Result<()>) -> Result<()> {
        let path = self.dir.join("yeda.toml");
        let mut table: toml::Table =
            if path.exists() { toml::from_str(&fs::read_to_string(&path)?)? } else { toml::Table::new() };
        edit(&mut table)?;
        fs::create_dir_all(&self.dir)?;
        fs::write(path, toml::to_string(&table)?)?;
        Ok(())
    }

    /// The named profile, else `default_llm`, else the only profile configured.
    pub fn llm_profile(&self, name: Option<&str>) -> Result<(String, Profile)> {
        let name = match (name, &self.default_llm) {
            (Some(n), _) => n.to_string(),
            (None, Some(d)) => d.clone(),
            (None, None) if self.llm.len() == 1 => self.llm.keys().next().unwrap().clone(),
            (None, None) if self.llm.is_empty() => bail!("no [llm.<name>] profile configured"),
            (None, None) => bail!("several llm profiles; set default_llm or name one ({})", self.llm.keys().cloned().collect::<Vec<_>>().join(", ")),
        };
        let p = self.llm.get(&name).with_context(|| format!("no llm profile named '{name}'"))?;
        Ok((name, p.clone()))
    }

    pub fn ref_path(&self, key: &str) -> PathBuf {
        self.dir.join("refs").join(format!("{key}.toml"))
    }

    pub fn annotations_dir(&self, key: &str) -> PathBuf {
        self.dir.join("annotations").join(key)
    }

    pub fn unique_key(&self, r: &Ref) -> String {
        let surname = r.authors.first().and_then(|a| a.split(',').next()).unwrap_or("anon");
        let mut base: String = surname.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect();
        if base.is_empty() {
            base = "anon".into();
        }
        if let Some(y) = r.year {
            base += &y.to_string();
        }
        let taken = |k: &str| self.refs.iter().any(|x| x.key == k) || self.ref_path(k).exists();
        std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}_{n}")))
            .find(|k| !taken(k))
            .unwrap()
    }

    pub fn save(&self, r: &Ref) -> Result<()> {
        fs::create_dir_all(self.dir.join("refs"))?;
        fs::write(self.ref_path(&r.key), toml::to_string(r)?)?;
        Ok(())
    }

    pub fn pdf_path(&self, r: &Ref) -> Result<PathBuf> {
        let pdf = r.pdf.as_ref().context("no pdf")?;
        let base = self
            .locations
            .get(&pdf.location)
            .with_context(|| format!("unknown pdf location '{}'", pdf.location))?;
        Ok(base.join(&pdf.path))
    }
}

pub fn config_section<'a>(table: &'a mut toml::Table, name: &str) -> Result<&'a mut toml::Table> {
    table
        .entry(name)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .with_context(|| format!("[{name}] in yeda.toml is not a table"))
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => p.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("yeda-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let lib = Library {
            dir: dir.clone(),
            locations: BTreeMap::new(),
            tag_colors: BTreeMap::new(),
            searches: BTreeMap::new(),
            reader: None,
            tables: BTreeMap::new(),
            llm: BTreeMap::new(),
            default_llm: None,
            refs: vec![],
        };
        let mut r = Ref {
            key: "smith2020".into(),
            kind: "article".into(),
            title: "A Title".into(),
            authors: vec!["Smith, J.".into()],
            year: Some(2020),
            tags: vec!["ml".into()],
            collections: vec!["thesis/ch2".into()],
            status: Some("To Read".into()),
            tables: BTreeMap::from([("lit".into(), BTreeMap::from([("hardware/sim".into(), "ROS".into())]))]),
            pdf: Some(Pdf { location: "papers".into(), path: "smith.pdf".into() }),
            ..Default::default()
        };
        r.extra.insert("journal".into(), toml::Value::String("J".into()));
        lib.save(&r).unwrap();
        let loaded = Library::load(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded.refs, vec![r]);
    }
}
