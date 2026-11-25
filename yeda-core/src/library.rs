use crate::Effect;
use crate::llm::{Bound, Profile, Provider};
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
    /// Names the PDF's annotations folder, `annotations/<key>/<id>/`; never reused within a reference.
    pub id: String,
    pub location: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// One of `STATUSES`; a file without one reads as "New".
    pub status: String,
    #[serde(rename = "pdf", skip_serializing_if = "Vec::is_empty")]
    pub pdfs: Vec<Pdf>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
    /// This reference's row in each table, by table name then column; kept in `ai_path`.
    #[serde(skip)]
    pub tables: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for Ref {
    fn default() -> Self {
        Ref {
            key: String::new(),
            kind: String::new(),
            title: String::new(),
            authors: Vec::new(),
            year: None,
            doi: None,
            tags: Vec::new(),
            collections: Vec::new(),
            status: STATUSES[0].into(),
            pdfs: Vec::new(),
            extra: BTreeMap::new(),
            tables: BTreeMap::new(),
        }
    }
}

/// A WebDAV server holding PDFs as plain files, `<citekey>/<file>` like a local location. The
/// password is never part of the library: clients keep it in the system secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dav {
    pub url: String,
    pub user: String,
    /// Distinguishes this location's environment variable from others on the same server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// A Dropbox folder holding PDFs. Its sign-in token is never part of the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cloud {
    /// Where PDFs go, inside yeda's Dropbox app folder.
    #[serde(default)]
    pub folder: String,
    /// Replaces the Dropbox app yeda was built with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl Cloud {
    /// The folder's segments, without the slashes around them.
    pub fn folder_segments(&self) -> Vec<&str> {
        self.folder.split('/').filter(|s| !s.is_empty()).collect()
    }
}

/// A `[locations.<name>]` table: where PDFs live other than in a local folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Remote {
    Webdav(Dav),
    Dropbox(Cloud),
}

/// Every `kind` a `[locations.<name>]` table may have.
pub const LOCATION_KINDS: [&str; 3] = ["local", "webdav", "dropbox"];

impl Remote {
    pub fn kind(&self) -> &'static str {
        match self {
            Remote::Webdav(_) => "webdav",
            Remote::Dropbox(_) => "dropbox",
        }
    }

    pub fn alias(&self) -> Option<&str> {
        match self {
            Remote::Webdav(d) => d.alias.as_deref(),
            _ => None,
        }
    }

    /// One line for lists: what the location is and where it points.
    pub fn label(&self) -> String {
        match self {
            Remote::Webdav(d) => format!("WebDAV {}@{}", d.user, d.url),
            Remote::Dropbox(c) => format!("Dropbox /{}", c.folder.trim_matches('/')),
        }
    }
}

impl Dav {
    /// The URL of `path` (`/`-separated), each segment percent-encoded.
    pub fn file_url(&self, path: &str) -> String {
        let encode = |seg: &str| {
            seg.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        };
        let path: Vec<String> = path.split('/').map(encode).collect();
        format!("{}/{}", self.url.trim_end_matches('/'), path.join("/"))
    }
}

/// A `[searches]` entry: the query, and the order its results are shown in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<crate::browse::SortField>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
}

impl SavedSearch {
    pub fn order(&self) -> Option<crate::browse::Sort> {
        self.sort.map(|field| crate::browse::Sort { field, descending: self.descending })
    }
}

/// A `[locations]` entry: a `kind = "local"` folder, or a remote.
enum Location {
    Folder(String),
    Remote(Box<Remote>),
}

/// The `[locations.<name>]` table for the local folder `path`.
pub fn local_location(path: &str) -> toml::Value {
    toml::Table::from_iter([("kind".to_string(), "local".into()), ("path".to_string(), path.into())]).into()
}

fn parse_location(name: &str, value: toml::Value) -> Result<Location> {
    let hint = || format!("locations.{name}: a location is a table with kind = {}", LOCATION_KINDS.map(|k| format!("\"{k}\"")).join(", "));
    let toml::Value::Table(t) = value else { bail!("{}", hint()) };
    match t.get("kind").and_then(|k| k.as_str()) {
        Some("local") => {
            let path = t.get("path").and_then(|p| p.as_str()).filter(|p| !p.is_empty());
            Ok(Location::Folder(path.with_context(|| format!("locations.{name}: a local location needs a path"))?.to_string()))
        }
        Some(kind) if LOCATION_KINDS.contains(&kind) => Ok(Location::Remote(Box::new(Remote::deserialize(toml::Value::Table(t)).with_context(|| format!("locations.{name}"))?))),
        _ => bail!("{}", hint()),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Config {
    locations: BTreeMap<String, toml::Value>,
    tag_colors: BTreeMap<String, String>,
    searches: BTreeMap<String, SavedSearch>,
    reader: Option<String>,
    tables: BTreeMap<String, Table>,
    providers: BTreeMap<String, Provider>,
    llm: BTreeMap<String, Profile>,
    default_llm: Option<String>,
    graph: Option<crate::graph::Settings>,
    fulltext: Option<crate::fulltext::Settings>,
    cite: Option<CiteSettings>,
    web_search: Option<toml::Table>,
}

/// `[cite]` in yeda.toml.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CiteSettings {
    /// What `:cite` copies without an argument: `bibtex`, a style name, or a `.csl` file.
    pub format: Option<String>,
}

/// The keys of the device config; `locations` may also be in `yeda.toml`.
const DEVICE_KEYS: [&str; 4] = ["library", "reader", "keys", "locations"];

pub const STATUSES: [&str; 5] = ["New", "To Read", "In Progress", "Read", "Not Reading"];

#[derive(Debug, Default)]
pub struct Library {
    /// Local folders, by location name.
    pub locations: BTreeMap<String, PathBuf>,
    /// WebDAV and cloud drive locations, by name.
    pub remotes: BTreeMap<String, Remote>,
    /// Where the client keeps downloaded copies of remote PDFs: `<cache>/<location>/<key>/<file>`.
    pub cache: Option<PathBuf>,
    pub tag_colors: BTreeMap<String, String>,
    pub searches: BTreeMap<String, SavedSearch>,
    /// Command that opens PDFs as `<reader> <pdf> --annotations <dir>`; clients default to yeda-reader.
    pub reader: Option<String>,
    pub tables: BTreeMap<String, Table>,
    /// `[providers.<name>]`: where and how to reach each LLM server.
    pub providers: BTreeMap<String, Provider>,
    pub llm: BTreeMap<String, Profile>,
    pub default_llm: Option<String>,
    /// `[graph]`, for citation graphs.
    pub graph: crate::graph::Settings,
    /// `[fulltext]`, for fetching PDFs.
    pub fulltext: crate::fulltext::Settings,
    /// `[web_search]`, for `:discover`.
    pub web_search: crate::web_search::Settings,
    pub cite: CiteSettings,
    /// Config the user should know about but that does not stop the load.
    pub warnings: Vec<String>,
    pub refs: Vec<Ref>,
    /// References left out because a file of theirs doesn't parse, with the error. Their keys stay
    /// taken, so nothing overwrites those files.
    pub unreadable: Vec<(String, String)>,
    /// `yeda.toml` as loaded, so config edits keep keys this version doesn't know.
    shared: toml::Table,
}

pub struct RemotePdf<'a> {
    pub location: &'a str,
    pub remote: &'a Remote,
    /// `/`-separated, relative to the remote's folder.
    pub path: String,
}

pub fn ref_path(key: &str) -> String {
    format!("{REFS}/{key}.toml")
}

pub fn annotations_dir(key: &str) -> String {
    format!("annotations/{key}")
}

/// The annotations folder of one of `key`'s PDFs.
pub fn pdf_annotations_dir(key: &str, pdf: &Pdf) -> String {
    format!("annotations/{key}/{}", pdf.id)
}

pub fn note_path(key: &str) -> String {
    format!("notes/{key}.md")
}

/// Everything an LLM wrote about the paper: its row in each table.
pub fn ai_path(key: &str) -> String {
    format!("notes/{key}{AI}")
}

const AI: &str = ".ai.toml";

/// The citekey of a `notes/` file name, or `None` if it isn't an LLM file.
pub fn ai_key(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(AI).filter(|k| valid_key(k))
}

/// The citekey of a `refs/` file name, or `None` if it isn't a reference file.
pub fn ref_key(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".toml").filter(|k| valid_key(k))
}

/// Whether `key` is safe as a file and folder name: `.` and `..` would escape their folders.
pub fn valid_key(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_alphanumeric()) && key.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c))
}

impl Library {
    /// `shared` is `yeda.toml`, `machine` the device config, `refs` each `(key, contents)` of
    /// `refs/`; `~/` in locations expands to `home`. Each key belongs to one of the two files, except
    /// `[locations]`, where the device config gives this device's folders.
    pub fn parse(
        shared: Option<&str>,
        machine: Option<&str>,
        refs: impl IntoIterator<Item = (String, String)>,
        home: Option<&Path>,
    ) -> Result<Self> {
        let shared_table: toml::Table = shared.map(toml::from_str).transpose().context(CONFIG)?.unwrap_or_default();
        if let Some(key) = shared_table.keys().find(|k| DEVICE_KEYS.contains(&k.as_str()) && *k != "locations") {
            bail!("{CONFIG}: `{key}` belongs in the device config (~/.config/yeda/config.toml)");
        }
        let machine_table: toml::Table = machine.map(toml::from_str).transpose().context("device config")?.unwrap_or_default();
        if let Some(key) = machine_table.keys().find(|k| !DEVICE_KEYS.contains(&k.as_str())) {
            bail!("device config: `{key}` belongs in the library's {CONFIG}");
        }
        let mut lib = Library { shared: shared_table, ..Default::default() };
        for (source, text) in [(CONFIG, shared), ("device config", machine)] {
            let Some(text) = text else { continue };
            let cfg: Config = toml::from_str(text).context(source)?;
            // A device config may turn a shared remote location into a local folder, or back.
            for (name, value) in cfg.locations {
                match parse_location(&name, value).context(source)? {
                    Location::Folder(p) => {
                        lib.remotes.remove(&name);
                        lib.locations.insert(name, expand_home(&p, home));
                    }
                    Location::Remote(remote) => {
                        lib.locations.remove(&name);
                        lib.remotes.insert(name, *remote);
                    }
                }
            }
            lib.tag_colors.extend(cfg.tag_colors);
            lib.searches.extend(cfg.searches);
            lib.reader = cfg.reader.or(lib.reader);
            lib.tables.extend(cfg.tables);
            lib.providers.extend(cfg.providers);
            lib.llm.extend(cfg.llm);
            lib.default_llm = cfg.default_llm.or(lib.default_llm);
            if let Some(g) = cfg.graph {
                lib.graph = g;
            }
            if let Some(f) = cfg.fulltext {
                lib.fulltext = f;
            }
            if let Some(c) = cfg.cite {
                lib.cite = c;
            }
            if let Some(t) = cfg.web_search {
                let (settings, warnings) = crate::web_search::Settings::parse(Some(&t)).context(CONFIG)?;
                lib.web_search = settings;
                lib.warnings = warnings;
            }
        }
        for (name, p) in &lib.providers {
            p.check().with_context(|| format!("providers.{name}"))?;
        }
        crate::check_aliases(lib.providers.iter().map(|(n, p)| (n.as_str(), p.url(), p.alias.as_deref()))).context("providers")?;
        let dav_aliases = lib.remotes.iter().filter_map(|(n, r)| match r {
            Remote::Webdav(d) => Some((n.as_str(), d.url.as_str(), d.alias.as_deref())),
            _ => None,
        });
        crate::check_aliases(dav_aliases).context("locations")?;
        for (key, text) in refs {
            match parse_ref(key.clone(), &text) {
                Ok(r) => lib.refs.push(r),
                Err(e) => lib.unreadable.push((key, format!("{e:#}"))),
            }
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

    /// Saves the search as `name`, replacing `old` when it is renamed.
    pub fn save_search(&mut self, name: &str, saved: &SavedSearch, old: Option<&str>) -> Result<Effect> {
        let value = toml::Value::try_from(saved)?;
        self.update_config(|t| {
            let searches = config_section(t, "searches")?;
            if let Some(old) = old {
                searches.remove(old);
            }
            searches.insert(name.into(), value);
            Ok(())
        })
    }

    /// The named profile, else `default_llm`, else the only profile configured, bound to its provider.
    pub fn llm_profile(&self, name: Option<&str>) -> Result<(String, Bound)> {
        let name = match (name, &self.default_llm) {
            (Some(n), _) => n.to_string(),
            (None, Some(d)) => d.clone(),
            (None, None) if self.llm.len() == 1 => self.llm.keys().next().unwrap().clone(),
            (None, None) if self.llm.is_empty() => bail!("no [llm.<name>] profile configured"),
            (None, None) => bail!("several llm profiles ({}); set default_llm or pass --llm <profile>", self.llm.keys().cloned().collect::<Vec<_>>().join(", ")),
        };
        let p = self.llm.get(&name).with_context(|| format!("no llm profile named '{name}'"))?;
        let settings = self
            .providers
            .get(&p.provider)
            .with_context(|| format!("llm.{name} names provider '{}', which is not in [providers.<name>]", p.provider))?;
        if p.model.trim().is_empty() {
            bail!("llm.{name}: give the model's name");
        }
        Ok((name.clone(), Bound::new(&name, p, &p.provider, settings)))
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
            .find(|k| !self.key_taken(k))
            .unwrap()
    }

    pub fn save(&self, r: &Ref) -> Result<Effect> {
        Ok(Effect::write(ref_path(&r.key), toml::to_string(r)?))
    }

    pub fn save_ai(&self, r: &Ref) -> Result<Effect> {
        if r.tables.is_empty() {
            return Ok(Effect::remove(ai_path(&r.key)));
        }
        Ok(Effect::write(ai_path(&r.key), toml::to_string(&r.tables)?))
    }

    /// Renames a table in yeda.toml and in every paper's LLM file.
    pub fn rename_table(&mut self, old: &str, new: &str) -> Result<Vec<Effect>> {
        if new.is_empty() || self.tables.contains_key(new) {
            bail!("there is a table named '{new}' already");
        }
        let in_shared = self.shared.get("tables").and_then(|t| t.get(old)).is_some();
        if !in_shared {
            bail!("no table '{old}' in {CONFIG}");
        }
        let mut effects = vec![self.update_config(|t| {
            let tables = config_section(t, "tables")?;
            let def = tables.remove(old).context("no such table")?;
            tables.insert(new.into(), def);
            Ok(())
        })?];
        for r in self.refs.iter().filter(|r| r.tables.contains_key(old)) {
            let mut r = r.clone();
            let row = r.tables.remove(old).unwrap_or_default();
            r.tables.insert(new.into(), row);
            effects.push(self.save_ai(&r)?);
        }
        Ok(effects)
    }

    /// Reads `notes/<key>.ai.toml` into the reference's tables; a file without its reference is ignored.
    pub fn load_ai(&mut self, key: &str, text: &str) {
        let Some(i) = self.refs.iter().position(|r| r.key == key) else { return };
        match toml::from_str(text) {
            Ok(tables) => self.refs[i].tables = tables,
            Err(e) => {
                self.refs.remove(i);
                self.unreadable.push((key.into(), format!("{}: {e}", ai_path(key))));
            }
        }
    }

    pub fn key_taken(&self, key: &str) -> bool {
        self.get(key).is_some() || self.unreadable.iter().any(|(k, _)| k == key)
    }

    /// Removes a reference with its annotations and notes. `key` is checked, since a client may
    /// take it from outside the library: `..` would remove a folder above it.
    pub fn delete(&self, key: &str) -> Result<Vec<Effect>> {
        anyhow::ensure!(valid_key(key), "invalid citekey {key:?}");
        Ok(vec![Effect::remove(ref_path(key)), Effect::remove(annotations_dir(key)), Effect::remove(note_path(key)), Effect::remove(ai_path(key))])
    }

    /// Where the PDF is on this machine; for a remote location, its place in `cache`, which may not
    /// have been downloaded yet.
    pub fn pdf_path(&self, pdf: &Pdf) -> Result<PathBuf> {
        if let Some(base) = self.locations.get(&pdf.location) {
            return Ok(base.join(&pdf.path));
        }
        anyhow::ensure!(self.remotes.contains_key(&pdf.location), "unknown pdf location '{}'", pdf.location);
        let cache = self.cache.as_ref().context("no cache folder for remote PDFs")?;
        Ok(cache.join(&pdf.location).join(&pdf.path))
    }

    /// The remote holding `pdf`, with its path on it.
    pub fn remote_pdf(&self, pdf: &Pdf) -> Option<RemotePdf<'_>> {
        let (name, remote) = self.remotes.get_key_value(&pdf.location)?;
        Some(RemotePdf { location: name, remote, path: pdf.path.clone() })
    }

    /// Where a PDF named `file_name` goes when attached to `r` at `location`: `<citekey>/<file name>`,
    /// with a new id. Fails if another of `r`'s PDFs is already there.
    pub fn attach(&self, r: &Ref, location: &str, file_name: &str) -> Result<Pdf> {
        anyhow::ensure!(
            self.locations.contains_key(location) || self.remotes.contains_key(location),
            "no location named '{location}' (locations: {})",
            self.locations.keys().chain(self.remotes.keys()).cloned().collect::<Vec<_>>().join(", ")
        );
        let file = Path::new(file_name).file_name().and_then(|f| f.to_str()).context("not a file name")?;
        let path = format!("{}/{file}", r.key);
        anyhow::ensure!(!r.pdfs.iter().any(|p| p.location == location && p.path == path), "{} already has {location}:{path}", r.key);
        Ok(Pdf { id: next_pdf_id(r), location: location.into(), path })
    }
}

pub fn parse_ref(key: String, text: &str) -> Result<Ref> {
    anyhow::ensure!(valid_key(&key), "invalid citekey {key:?}");
    let mut r: Ref = toml::from_str(text).with_context(|| ref_path(&key))?;
    for pdf in &r.pdfs {
        // Reference files come from other devices; their PDF must stay inside its location, and
        // its annotations inside the reference's folder.
        let inside = Path::new(&pdf.path).components().all(|c| matches!(c, std::path::Component::Normal(_)));
        anyhow::ensure!(inside, "{}: pdf path {:?} leaves its location", ref_path(&key), pdf.path);
        anyhow::ensure!(valid_pdf_id(&pdf.id), "{}: invalid pdf id {:?}", ref_path(&key), pdf.id);
    }
    let mut ids: Vec<&str> = r.pdfs.iter().map(|p| p.id.as_str()).collect();
    ids.sort();
    anyhow::ensure!(ids.windows(2).all(|w| w[0] != w[1]), "{}: two PDFs share an id", ref_path(&key));
    r.key = key;
    Ok(r)
}

fn valid_pdf_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn next_pdf_id(r: &Ref) -> String {
    (r.pdfs.iter().filter_map(|p| p.id.parse::<u32>().ok()).max().unwrap_or(0) + 1).to_string()
}

/// The import date belongs to the old status, so it goes.
pub fn set_status(r: &mut Ref, status: String) {
    r.status = status;
    r.extra.remove("status_date");
}

/// Steps through `STATUSES`; an unknown status steps from "New".
pub fn cycle_status(r: &mut Ref, forward: bool) {
    let n = STATUSES.len();
    let cur = STATUSES.iter().position(|x| *x == r.status).unwrap_or(0);
    let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
    set_status(r, STATUSES[next].to_string());
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
    #[test]
    fn rejects_keys_that_escape_their_folder() {
        assert_eq!(ref_key("smith2020.toml"), Some("smith2020"));
        assert_eq!(ref_key("...toml"), None);
        assert_eq!(ref_key("..toml"), None);
        assert!(!valid_key("a/b"));
        assert!(parse_ref("../x".into(), "title = \"A\"\n").is_err());
        assert!(parse_ref("".into(), "title = \"A\"\n").is_err());
        let lib = Library::default();
        assert!(lib.delete("../..").is_err());
        assert!(lib.delete("a/b").is_err());
        assert_eq!(lib.delete("a2020").unwrap().len(), 4);
    }

    #[test]
    fn renames_tables() {
        let refs = [("a", "title = \"A\"\n"), ("b", "title = \"B\"\n")].map(|(k, t)| (k.to_string(), t.to_string()));
        let mut lib = Library::parse(Some("[tables.lit]\ncolumns = [\"m\"]\n[tables.other]\ncolumns = []\n"), None, refs, None).unwrap();
        lib.load_ai("a", "[lit]\nm = \"x\"\n[other]\n");
        assert!(lib.rename_table("lit", "other").is_err());
        assert!(lib.rename_table("nope", "phd").is_err());
        let effects = lib.rename_table("lit", "phd").unwrap();
        assert_eq!(effects.len(), 2);
        let text = |e: &Effect| match e {
            Effect::Write { bytes, .. } => String::from_utf8(bytes.clone()).unwrap(),
            _ => panic!(),
        };
        assert!(text(&effects[0]).contains("[tables.phd]") && !text(&effects[0]).contains("[tables.lit]"));
        assert!(text(&effects[1]).contains("[phd]") && text(&effects[1]).contains("m = \"x\""));
    }

    #[test]
    fn saves_searches_with_their_order() {
        let mut lib = Library::parse(Some("[searches]\nold = { query = \"tag:x\" }\n"), None, [], None).unwrap();
        assert_eq!(lib.searches["old"].order(), None);
        let saved = SavedSearch { query: "year:2020".into(), sort: Some(crate::browse::SortField::Year), descending: true };
        let Effect::Write { bytes, .. } = lib.save_search("new", &saved, Some("old")).unwrap() else { panic!() };
        let lib = Library::parse(Some(std::str::from_utf8(&bytes).unwrap()), None, [], None).unwrap();
        assert_eq!(lib.searches.keys().collect::<Vec<_>>(), ["new"]);
        assert_eq!(lib.searches["new"], saved);
    }

    use super::*;

    #[test]
    fn binds_profiles_to_their_provider_and_refuses_a_missing_one() {
        let cfg = "[providers.claude]\nbase_url = \"https://api.anthropic.com\"\nprotocol = \"anthropic\"\nauth = \"oauth\"\n[llm.a]\nprovider = \"claude\"\nmodel = \"claude-opus-5-5\"\n[llm.b]\nprovider = \"claude\"\nmodel = \"claude-sonnet-5-5\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let (name, a) = lib.llm_profile(Some("a")).unwrap();
        let (_, b) = lib.llm_profile(Some("b")).unwrap();
        assert_eq!((name.as_str(), a.url(), b.url()), ("a", "https://api.anthropic.com", "https://api.anthropic.com"));
        assert_eq!((a.model.as_str(), b.model.as_str()), ("claude-opus-5-5", "claude-sonnet-5-5"));

        let orphan = Library::parse(Some("[llm.a]\nprovider = \"nowhere\"\nmodel = \"m\"\n"), None, [], None).unwrap();
        let err = orphan.llm_profile(Some("a")).unwrap_err().to_string();
        assert!(err.contains("names provider 'nowhere'"), "{err}");
        let broken = Library::parse(Some("[providers.x]\nbase_url = \"http://x.test/v1\"\nprotocol = \"openai\"\nauth = \"api-key\"\n"), None, [], None);
        assert!(format!("{:#}", broken.unwrap_err()).contains("providers.x: http://x.test/v1"));
    }

    #[test]
    fn rejects_pdf_paths_outside_their_location() {
        let pdf = |path: &str| parse_ref("a".into(), &format!("title = \"A\"\n[[pdf]]\nid = \"1\"\nlocation = \"dav\"\npath = \"{path}\"\n"));
        assert!(pdf("a/a.pdf").is_ok());
        assert!(pdf("../../.bashrc").is_err());
        assert!(pdf("/etc/passwd").is_err());
    }

    #[test]
    fn leaves_out_unreadable_references_but_keeps_their_keys() {
        let refs = [("a", "title = \"A\"\n"), ("smith2020", "title = \n")].map(|(k, t)| (k.to_string(), t.to_string()));
        let mut lib = Library::parse(None, None, refs, None).unwrap();
        lib.load_ai("a", "not toml [");
        assert!(lib.refs.is_empty());
        assert_eq!(lib.unreadable.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["smith2020", "a"]);
        let r = Ref { authors: vec!["Smith, J.".into()], year: Some(2020), ..Default::default() };
        assert_eq!(lib.unique_key(&r), "smith2020_2");
    }

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
            status: "To Read".into(),
            tables: BTreeMap::from([("lit".into(), BTreeMap::from([("hardware/sim".into(), "ROS".into())]))]),
            pdfs: vec![Pdf { id: "1".into(), location: "papers".into(), path: "smith.pdf".into() }],
            ..Default::default()
        };
        r.extra.insert("journal".into(), toml::Value::String("J".into()));
        let Effect::Write { path, bytes } = Library::default().save(&r).unwrap() else { panic!() };
        assert_eq!(path, "refs/smith2020.toml");
        assert!(!String::from_utf8_lossy(&bytes).contains("ROS"));
        let Effect::Write { path: ai, bytes: ai_bytes } = Library::default().save_ai(&r).unwrap() else { panic!() };
        assert_eq!(ai, "notes/smith2020.ai.toml");
        assert_eq!(ai_key("smith2020.ai.toml"), Some("smith2020"));
        let shared = "[locations.papers]\nkind = \"local\"\npath = \"~/papers\"\n";
        let mut lib = Library::parse(Some(shared), None, [("smith2020".into(), String::from_utf8(bytes).unwrap())], Some(Path::new("/h"))).unwrap();
        lib.load_ai("smith2020", &String::from_utf8(ai_bytes).unwrap());
        assert_eq!(lib.refs, vec![r]);
        assert_eq!(lib.pdf_path(&lib.refs[0].pdfs[0]).unwrap(), Path::new("/h/papers/smith.pdf"));
    }

    #[test]
    fn keys_belong_to_one_file() {
        assert!(Library::parse(Some("reader = \"zathura\"\n"), None, [], None).is_err());
        assert!(Library::parse(None, Some("default_llm = \"claude\"\n"), [], None).is_err());
        let lib = Library::parse(Some("default_llm = \"claude\"\n"), Some("library = \"~/l\"\nreader = \"zathura\"\n[keys.normal]\nx = \"\"\n"), [], None).unwrap();
        assert_eq!(lib.reader.as_deref(), Some("zathura"));
    }

    #[test]
    fn device_folders_win_and_edits_keep_unknown_keys() {
        let mut lib = Library::parse(
            Some("future_key = 1\n[locations.papers]\nkind = \"local\"\npath = \"/shared\"\n"),
            Some("[locations.papers]\nkind = \"local\"\npath = \"/mine\"\n"),
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
        assert!(text.contains("future_key = 1") && text.contains("path = \"/shared\"") && text.contains("ml = \"red\""));
    }

    #[test]
    fn cycles_status_from_new() {
        let mut r = Ref::default();
        assert_eq!(parse_ref("a".into(), "title = \"A\"\n").unwrap().status, "New");
        cycle_status(&mut r, false);
        assert_eq!(r.status, "Not Reading");
        cycle_status(&mut r, true);
        assert_eq!(r.status, "New");
    }

    #[test]
    fn webdav_locations_and_attachments() {
        let shared = "[locations.papers]\nkind = \"local\"\npath = \"/p\"\n\n[locations.dav]\nkind = \"webdav\"\nurl = \"https://dav.example/zotero/\"\nuser = \"me\"\n";
        let refs = [("a2020".to_string(), "[[pdf]]\nid = \"1\"\nlocation = \"dav\"\npath = \"ABCD2345/a.pdf\"\n".to_string())];
        let mut lib = Library::parse(Some(shared), None, refs, None).unwrap();
        assert!(lib.pdf_path(&lib.refs[0].pdfs[0]).is_err());
        lib.cache = Some("/c".into());
        assert_eq!(lib.pdf_path(&lib.refs[0].pdfs[0]).unwrap(), Path::new("/c/dav/ABCD2345/a.pdf"));
        let remote = lib.remote_pdf(&lib.refs[0].pdfs[0]).unwrap();
        assert_eq!(remote.path, "ABCD2345/a.pdf");
        let Remote::Webdav(dav) = remote.remote else { panic!() };
        assert_eq!(dav.file_url("a2020/Smith - 2020 - A & B.pdf"), "https://dav.example/zotero/a2020/Smith%20-%202020%20-%20A%20%26%20B.pdf");

        let pdf = lib.attach(&lib.refs[0], "papers", "/tmp/x/Paper.pdf").unwrap();
        assert_eq!((pdf.id.as_str(), pdf.location.as_str(), pdf.path.as_str()), ("2", "papers", "a2020/Paper.pdf"));
        let mut r = lib.refs[0].clone();
        r.pdfs.push(pdf);
        assert!(lib.attach(&r, "papers", "Paper.pdf").is_err());
        assert!(lib.attach(&lib.refs[0], "nowhere", "x.pdf").is_err());

        let local = Library::parse(Some(shared), Some("[locations.dav]\nkind = \"local\"\npath = \"/mirror\"\n"), [], None).unwrap();
        assert!(local.remotes.is_empty());
        assert_eq!(local.locations["dav"], Path::new("/mirror"));
    }

    #[test]
    fn locations_need_a_known_kind() {
        let shared = "[locations.box]\nkind = \"dropbox\"\nfolder = \"/yeda\"\nclient_id = \"mine\"\n";
        let lib = Library::parse(Some(shared), None, [], None).unwrap();
        let Remote::Dropbox(box_) = &lib.remotes["box"] else { panic!() };
        assert_eq!((box_.folder_segments(), box_.client_id.as_deref()), (vec!["yeda"], Some("mine")));
        assert_eq!(lib.remotes["box"].label(), "Dropbox /yeda");

        let bad = [
            "[locations.a]\nurl = \"https://x/\"\nuser = \"u\"\n",
            "[locations.a]\nkind = \"ftp\"\n",
            "[locations.a]\nkind = \"gdrive\"\nfolder = \"x\"\n",
            "[locations.a]\nkind = \"local\"\n",
            "[locations]\na = \"/bare/string\"\n",
        ];
        for text in bad {
            let err = format!("{:#}", Library::parse(Some(text), None, [], None).unwrap_err());
            assert!(err.contains("locations.a"), "{err}");
        }
    }
}
