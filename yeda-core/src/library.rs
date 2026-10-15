use crate::Effect;
use crate::llm::Profile;
use crate::tables::Table;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The shared config file at the library root.
pub const CONFIG: &str = "yeda.toml";
pub const REFS: &str = "refs";

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

#[derive(Debug, Default)]
pub struct Library {
    pub locations: BTreeMap<String, PathBuf>,
    pub tag_colors: BTreeMap<String, String>,
    pub searches: BTreeMap<String, String>,
    /// Command that opens PDFs as `<reader> <pdf> --annotations <dir>`; the system viewer when unset.
    pub reader: Option<String>,
    pub tables: BTreeMap<String, Table>,
    pub llm: BTreeMap<String, Profile>,
    pub default_llm: Option<String>,
    pub refs: Vec<Ref>,
    /// `yeda.toml` as loaded, so config edits keep keys this version doesn't know.
    shared: toml::Table,
}

pub fn ref_path(key: &str) -> String {
    format!("{REFS}/{key}.toml")
}

pub fn annotations_dir(key: &str) -> String {
    format!("annotations/{key}")
}

pub fn note_path(key: &str) -> String {
    format!("notes/{key}.md")
}

/// The citekey of a `refs/` file name, or `None` if it isn't a reference file.
pub fn ref_key(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".toml").filter(|k| !k.is_empty())
}

impl Library {
    /// `shared` is `yeda.toml`, `machine` the per-device config that overrides it, `refs` each
    /// `(key, contents)` of `refs/`; `~/` in locations expands to `home`.
    pub fn parse(
        shared: Option<&str>,
        machine: Option<&str>,
        refs: impl IntoIterator<Item = (String, String)>,
        home: Option<&Path>,
    ) -> Result<Self> {
        let shared_table: toml::Table = shared.map(toml::from_str).transpose().context(CONFIG)?.unwrap_or_default();
        let mut lib = Library { shared: shared_table, ..Default::default() };
        for (name, text) in [(CONFIG, shared), ("machine config", machine)] {
            let Some(text) = text else { continue };
            let cfg: Config = toml::from_str(text).context(name)?;
            for (name, p) in cfg.locations {
                lib.locations.insert(name, expand_home(&p, home));
            }
            lib.tag_colors.extend(cfg.tag_colors);
            lib.searches.extend(cfg.searches);
            lib.reader = cfg.reader.or(lib.reader);
            lib.tables.extend(cfg.tables);
            lib.llm.extend(cfg.llm);
            lib.default_llm = cfg.default_llm.or(lib.default_llm);
        }
        for (key, text) in refs {
            let mut r: Ref = toml::from_str(&text).with_context(|| ref_path(&key))?;
            r.key = key;
            lib.refs.push(r);
        }
        lib.refs.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(lib)
    }

    /// Edits the shared `yeda.toml` (never the machine-local config); the caller reloads after applying.
    // ponytail: rewrites the file through toml::Table, dropping comments; use toml_edit if that matters.
    pub fn update_config(&mut self, edit: impl FnOnce(&mut toml::Table) -> Result<()>) -> Result<Effect> {
        edit(&mut self.shared)?;
        Ok(Effect::write(CONFIG, toml::to_string(&self.shared)?))
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

    pub fn get(&self, key: &str) -> Option<&Ref> {
        self.refs.iter().find(|r| r.key == key)
    }

    pub fn find_doi(&self, doi: &str) -> Option<&Ref> {
        self.refs.iter().find(|r| r.doi.as_deref() == Some(doi))
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
        std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}_{n}")))
            .find(|k| self.get(k).is_none())
            .unwrap()
    }

    pub fn save(&self, r: &Ref) -> Result<Effect> {
        Ok(Effect::write(ref_path(&r.key), toml::to_string(r)?))
    }

    /// Removes a reference with its annotations and notes.
    pub fn delete(&self, key: &str) -> Vec<Effect> {
        vec![Effect::remove(ref_path(key)), Effect::remove(annotations_dir(key)), Effect::remove(note_path(key))]
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

/// Steps through `STATUSES` and "no status"; the import date belongs to the old status, so it goes.
pub fn cycle_status(r: &mut Ref, forward: bool) {
    let n = STATUSES.len() + 1;
    let cur = r.status.as_deref().and_then(|s| STATUSES.iter().position(|x| *x == s)).map_or(0, |p| p + 1);
    let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
    r.status = next.checked_sub(1).map(|p| STATUSES[p].to_string());
    r.extra.remove("status_date");
}

pub fn config_section<'a>(table: &'a mut toml::Table, name: &str) -> Result<&'a mut toml::Table> {
    table
        .entry(name)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .with_context(|| format!("[{name}] in {CONFIG} is not a table"))
}

fn expand_home(p: &str, home: Option<&Path>) -> PathBuf {
    match (p.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => p.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_parse_roundtrip() {
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
        let Effect::Write { path, bytes } = Library::default().save(&r).unwrap() else { panic!() };
        assert_eq!(path, "refs/smith2020.toml");
        let shared = "[locations]\npapers = \"~/papers\"\n";
        let lib = Library::parse(Some(shared), None, [("smith2020".into(), String::from_utf8(bytes).unwrap())], Some(Path::new("/h"))).unwrap();
        assert_eq!(lib.refs, vec![r]);
        assert_eq!(lib.pdf_path(&lib.refs[0]).unwrap(), Path::new("/h/papers/smith.pdf"));
    }

    #[test]
    fn machine_config_wins_and_edits_keep_unknown_keys() {
        let mut lib = Library::parse(
            Some("future_key = 1\n[locations]\npapers = \"/shared\"\n"),
            Some("[locations]\npapers = \"/mine\"\n"),
            [],
            None,
        )
        .unwrap();
        assert_eq!(lib.locations["papers"], Path::new("/mine"));
        let Effect::Write { bytes, .. } = lib
            .update_config(|t| {
                config_section(t, "tag_colors")?.insert("ml".into(), "red".into());
                Ok(())
            })
            .unwrap()
        else {
            panic!()
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("future_key = 1") && text.contains("papers = \"/shared\"") && text.contains("ml = \"red\""));
    }

    #[test]
    fn cycles_status_through_none() {
        let mut r = Ref::default();
        cycle_status(&mut r, false);
        assert_eq!(r.status.as_deref(), Some("Not Reading"));
        cycle_status(&mut r, true);
        assert_eq!(r.status, None);
    }
}
