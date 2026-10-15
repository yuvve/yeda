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

/// A WebDAV server holding PDFs in Zotero's layout (`<key>.zip` and `<key>.prop`). The password
/// is never part of the library: clients keep it in the system secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dav {
    pub url: String,
    pub user: String,
}

impl Dav {
    pub fn file_url(&self, name: &str) -> String {
        format!("{}/{name}", self.url.trim_end_matches('/'))
    }
}

/// A `[locations]` entry: a local folder, or a table describing a WebDAV server.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Location {
    Folder(String),
    Dav(Dav),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Config {
    locations: BTreeMap<String, Location>,
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
    /// Local folders, by location name.
    pub locations: BTreeMap<String, PathBuf>,
    /// WebDAV locations, by name.
    pub remotes: BTreeMap<String, Dav>,
    /// Where the client keeps downloaded copies of remote PDFs: `<cache>/<location>/<key>/<file>`.
    pub cache: Option<PathBuf>,
    pub tag_colors: BTreeMap<String, String>,
    pub searches: BTreeMap<String, String>,
    /// Command that opens PDFs as `<reader> <pdf> --annotations <dir>`; clients default to yeda-reader.
    pub reader: Option<String>,
    pub tables: BTreeMap<String, Table>,
    pub llm: BTreeMap<String, Profile>,
    pub default_llm: Option<String>,
    pub refs: Vec<Ref>,
    /// `yeda.toml` as loaded, so config edits keep keys this version doesn't know.
    shared: toml::Table,
}

pub struct RemotePdf<'a> {
    pub location: &'a str,
    pub dav: &'a Dav,
    pub key: String,
    pub file: String,
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
            // A machine config may turn a shared WebDAV location into a local folder, or back.
            for (name, location) in cfg.locations {
                match location {
                    Location::Folder(p) => {
                        lib.remotes.remove(&name);
                        lib.locations.insert(name, expand_home(&p, home));
                    }
                    Location::Dav(dav) => {
                        lib.locations.remove(&name);
                        lib.remotes.insert(name, dav);
                    }
                }
            }
            lib.tag_colors.extend(cfg.tag_colors);
            lib.searches.extend(cfg.searches);
            lib.reader = cfg.reader.or(lib.reader);
            lib.tables.extend(cfg.tables);
            lib.llm.extend(cfg.llm);
            lib.default_llm = cfg.default_llm.or(lib.default_llm);
        }
        for (key, text) in refs {
            lib.refs.push(parse_ref(key, &text)?);
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

    /// Where the PDF is on this machine; for a WebDAV location, its place in `cache`, which may not
    /// have been downloaded yet.
    pub fn pdf_path(&self, r: &Ref) -> Result<PathBuf> {
        let pdf = r.pdf.as_ref().context("no pdf")?;
        if let Some(base) = self.locations.get(&pdf.location) {
            return Ok(base.join(&pdf.path));
        }
        anyhow::ensure!(self.remotes.contains_key(&pdf.location), "unknown pdf location '{}'", pdf.location);
        let cache = self.cache.as_ref().context("no cache folder for WebDAV PDFs")?;
        Ok(cache.join(&pdf.location).join(&pdf.path))
    }

    /// The WebDAV server holding `r`'s PDF, with its attachment key and file name.
    pub fn remote_pdf(&self, r: &Ref) -> Option<RemotePdf<'_>> {
        let pdf = r.pdf.as_ref()?;
        let (name, dav) = self.remotes.get_key_value(&pdf.location)?;
        let (key, file) = pdf.path.split_once('/')?;
        Some(RemotePdf { location: name, dav, key: key.into(), file: file.into() })
    }

    /// Where a PDF named `file_name` goes when attached to `r` at `location`: `<key>/<file name>`,
    /// as Zotero lays out its storage, with a new Zotero-style attachment key.
    pub fn attach(&self, r: &Ref, location: &str, file_name: &str) -> Result<Pdf> {
        anyhow::ensure!(
            self.locations.contains_key(location) || self.remotes.contains_key(location),
            "no location named '{location}' (locations: {})",
            self.locations.keys().chain(self.remotes.keys()).cloned().collect::<Vec<_>>().join(", ")
        );
        let file = Path::new(file_name).file_name().and_then(|f| f.to_str()).context("not a file name")?;
        let key = crate::ids::zotero_key(&format!("{}|{location}|{file}", r.key));
        Ok(Pdf { location: location.into(), path: format!("{key}/{file}") })
    }
}

pub fn parse_ref(key: String, text: &str) -> Result<Ref> {
    let mut r: Ref = toml::from_str(text).with_context(|| ref_path(&key))?;
    r.key = key;
    Ok(r)
}

/// The import date belongs to the old status, so it goes.
pub fn set_status(r: &mut Ref, status: Option<String>) {
    r.status = status;
    r.extra.remove("status_date");
}

/// Steps through `STATUSES` and "no status".
pub fn cycle_status(r: &mut Ref, forward: bool) {
    let n = STATUSES.len() + 1;
    let cur = r.status.as_deref().and_then(|s| STATUSES.iter().position(|x| *x == s)).map_or(0, |p| p + 1);
    let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
    set_status(r, next.checked_sub(1).map(|p| STATUSES[p].to_string()));
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

    #[test]
    fn webdav_locations_and_attachments() {
        let shared = "[locations]\npapers = \"/p\"\n\n[locations.dav]\nurl = \"https://dav.example/zotero/\"\nuser = \"me\"\n";
        let refs = [("a2020".to_string(), "[pdf]\nlocation = \"dav\"\npath = \"ABCD2345/a.pdf\"\n".to_string())];
        let mut lib = Library::parse(Some(shared), None, refs, None).unwrap();
        assert!(lib.pdf_path(&lib.refs[0]).is_err());
        lib.cache = Some("/c".into());
        assert_eq!(lib.pdf_path(&lib.refs[0]).unwrap(), Path::new("/c/dav/ABCD2345/a.pdf"));
        let remote = lib.remote_pdf(&lib.refs[0]).unwrap();
        assert_eq!((remote.key.as_str(), remote.file.as_str()), ("ABCD2345", "a.pdf"));
        assert_eq!(remote.dav.file_url("ABCD2345.zip"), "https://dav.example/zotero/ABCD2345.zip");

        let pdf = lib.attach(&lib.refs[0], "papers", "/tmp/x/Paper.pdf").unwrap();
        assert_eq!(pdf.location, "papers");
        assert!(pdf.path.ends_with("/Paper.pdf") && pdf.path.len() == "ABCD2345/Paper.pdf".len(), "{}", pdf.path);
        assert!(lib.attach(&lib.refs[0], "nowhere", "x.pdf").is_err());

        let local = Library::parse(Some(shared), Some("[locations]\ndav = \"/mirror\"\n"), [], None).unwrap();
        assert!(local.remotes.is_empty());
        assert_eq!(local.locations["dav"], Path::new("/mirror"));
    }
}
