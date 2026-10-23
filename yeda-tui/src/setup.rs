use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;
use yeda_core::browse::SortField;
use yeda_core::library::{Dav, Library, Ref, SavedSearch};
use yeda_core::llm::{Profile, Provider};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Llm,
    Webdav,
    /// Editing a saved search; nothing to test.
    Search,
    /// The optional OpenAlex API key, which only goes to the secret store.
    Openalex,
    /// A reference typed in by hand; nothing to test.
    Reference,
    /// `[fulltext] proxies`; nothing to test, since a proxy wants a login.
    Proxy,
    /// A literature table: its name, columns, then one description per column.
    Table,
}

/// The `:setup` form for one LLM profile or WebDAV location, whose last field is the secret; a saved search being edited; or a new reference.
pub struct SetupForm {
    pub kind: Kind,
    pub values: Vec<String>,
    pub sel: usize,
    /// The connection is being tested; the form waits.
    pub testing: bool,
    /// The saved search's name before editing, or the key of the reference being duplicated.
    pub old: Option<String>,
    /// A table's columns that have description fields now, and every description typed, by column.
    shown: Vec<String>,
    described: BTreeMap<String, String>,
}

/// Providers the form knows: name, API and base URL.
pub const PRESETS: [(&str, Provider, &str); 5] = [
    ("anthropic", Provider::Anthropic, "https://api.anthropic.com"),
    ("openai", Provider::Openai, "https://api.openai.com/v1"),
    ("gemini", Provider::Openai, "https://generativelanguage.googleapis.com/v1beta/openai"),
    ("mistral", Provider::Openai, "https://api.mistral.ai/v1"),
    ("openrouter", Provider::Openai, "https://openrouter.ai/api/v1"),
];

fn preset(name: &str) -> Option<&'static (&'static str, Provider, &'static str)> {
    PRESETS.iter().find(|p| p.0 == name.trim().to_lowercase())
}

/// The reference form's last fields, kept in `Ref::extra`.
pub const EXTRAS: [&str; 4] = ["url", "journal", "publisher", "abstract"];

/// What the form saves, once its connection works.
#[derive(Clone, Debug)]
pub enum Setup {
    Llm(Profile),
    Webdav(Dav),
    Search(SavedSearch),
    Openalex,
    Reference(Ref),
    /// Saved as it is, with nothing to test.
    Untested,
}

impl SetupForm {
    /// Filled in from the profile or location `name`, if there is one.
    pub fn new(kind: Kind, name: &str, lib: &Library) -> SetupForm {
        let values = match kind {
            Kind::Llm => match lib.llm.get(name) {
                Some(p) => {
                    let builtin = if p.provider == Provider::Anthropic { &PRESETS[0] } else { &PRESETS[1] };
                    let url = p.base_url.clone().unwrap_or(builtin.2.into());
                    let provider = PRESETS.iter().find(|q| q.1 == p.provider && q.2 == url.trim_end_matches('/')).unwrap_or(builtin).0;
                    vec![name.into(), provider.into(), p.model.clone(), url, String::new()]
                }
                None => vec![name.into(), "anthropic".into(), String::new(), PRESETS[0].2.into(), String::new()],
            },
            Kind::Webdav => match lib.remotes.get(name) {
                Some(d) => vec![name.into(), d.url.clone(), d.user.clone(), String::new()],
                None => vec![name.into(), String::new(), String::new(), String::new()],
            },
            Kind::Search => {
                let s = lib.searches.get(name);
                let sort = s.and_then(|s| Some(format!("{}{}", if s.descending { "-" } else { "" }, sort_name(s.sort?))));
                vec![name.into(), s.map(|s| s.query.clone()).unwrap_or_default(), sort.unwrap_or_default()]
            }
            Kind::Openalex => vec![String::new()],
            Kind::Proxy => vec![lib.fulltext.proxies.join(" ")],
            Kind::Table => vec![name.into(), lib.tables.get(name).map(|t| t.columns.join(", ")).unwrap_or_default()],
            Kind::Reference => {
                let r = lib.get(name).cloned().unwrap_or(Ref { kind: "article".into(), ..Default::default() });
                let extra = |k: &str| r.extra.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
                vec![
                    r.kind.clone(),
                    r.title.clone(),
                    r.authors.join("; "),
                    r.year.map(|y| y.to_string()).unwrap_or_default(),
                    // A copy is another work, and a DOI names one.
                    String::new(),
                ]
                .into_iter()
                .chain(EXTRAS.map(extra))
                .collect()
            }
        };
        let sel = match kind {
            Kind::Reference => 1,
            Kind::Openalex | Kind::Proxy => 0,
            _ if name.is_empty() => 0,
            _ => 1,
        };
        let old = match kind {
            Kind::Search => Some(name.to_string()),
            Kind::Reference => Some(name.to_string()).filter(|n| !n.is_empty()),
            _ => None,
        };
        let described = if kind == Kind::Table { lib.tables.get(name).map(|t| t.describe.clone()).unwrap_or_default() } else { BTreeMap::new() };
        let mut form = SetupForm { kind, values, sel, testing: false, old, shown: Vec::new(), described };
        form.sync_columns();
        form
    }

    /// Gives each column in a table's Columns field a description field, with what was typed for that column before.
    pub fn sync_columns(&mut self) {
        if self.kind != Kind::Table {
            return;
        }
        for (column, text) in self.shown.iter().zip(&self.values[2..]) {
            self.described.insert(column.clone(), text.clone());
        }
        self.shown = self.values[1].split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect();
        self.values.truncate(2);
        self.values.extend(self.shown.iter().map(|c| self.described.get(c).cloned().unwrap_or_default()));
    }

    /// The saved-search form filled in from `name`, under a name not yet taken, saving a new search.
    pub fn duplicate(name: &str, lib: &Library) -> SetupForm {
        let mut form = SetupForm::new(Kind::Search, name, lib);
        let copy = std::iter::once(format!("{name} copy")).chain((2..).map(|n| format!("{name} copy {n}"))).find(|n| !lib.searches.contains_key(n));
        form.values[0] = copy.unwrap_or_default();
        form.sel = 0;
        form.old = None;
        form
    }

    /// Picks the next (or previous) provider preset, filling in its base URL.
    pub fn cycle_provider(&mut self, back: bool) {
        let at = PRESETS.iter().position(|p| p.0 == self.values[1].trim().to_lowercase());
        let n = PRESETS.len();
        let next = match (at, back) {
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
            (None, _) => 0,
        };
        self.values[1] = PRESETS[next].0.into();
        self.provider_changed();
    }

    /// A known provider replaces the base URL, unless one was typed that no preset has.
    pub fn provider_changed(&mut self) {
        let url = self.values[3].trim();
        if let Some(p) = preset(&self.values[1])
            && (url.is_empty() || PRESETS.iter().any(|q| q.2 == url))
        {
            self.values[3] = p.2.into();
        }
    }

    /// Each field's label and hint.
    pub fn fields(&self) -> Vec<(String, &'static str)> {
        let fields: &[(&str, &str)] = match self.kind {
            Kind::Table => {
                let head = [("Name".to_string(), "a new name makes a new table; :table rename renames one"), ("Columns".into(), "separated by commas")];
                let describe = self.shown.iter().map(|c| (c.clone(), "what the LLM puts in this column; optional"));
                return head.into_iter().chain(describe).collect();
            }
            Kind::Proxy => &[("Proxies", "tried in order, separated by spaces; {url} is where the paper's address goes")],
            Kind::Llm => &[
                ("Name", "used as --llm <name>"),
                ("Provider", "←/→: anthropic, openai, gemini, mistral, openrouter"),
                ("Model", "e.g. claude-opus-5-5"),
                ("Base URL", "filled in by the provider; with openai, any OpenAI-compatible server"),
                ("API key", "kept in the secret store; empty for a local server"),
            ],
            Kind::Webdav => &[
                ("Name", "used as pdf.location and in :attach"),
                ("URL", "the folder PDFs go in, e.g. https://dav.example.com/pdfs/"),
                ("User", ""),
                ("Password", "kept in the secret store"),
            ],
            Kind::Openalex => &[("API key", "free at openalex.org/settings/api; kept in the secret store")],
            Kind::Reference => &[
                ("Type", "article, inproceedings, incollection, book, preprint, or another, such as report or thesis"),
                ("Title", ""),
                ("Authors", "Surname, Given; separated by ;"),
                ("Year", ""),
                ("DOI", "optional, like the rest; e edits other fields later"),
                ("URL", ""),
                ("Journal", ""),
                ("Publisher", ""),
                ("Abstract", "Ctrl-G edits it in $EDITOR"),
            ],
            Kind::Search => &[
                ("Name", ""),
                ("Query", "as typed after /"),
                ("Sort", "key, title, author, year, status or type; -year for descending; empty for none"),
            ],
        };
        fields.iter().map(|(label, hint)| (label.to_string(), *hint)).collect()
    }

    pub fn name(&self) -> &str {
        match self.kind {
            Kind::Openalex => "openalex",
            Kind::Proxy => "proxy",
            _ => self.values[0].trim(),
        }
    }

    fn has_secret(&self) -> bool {
        matches!(self.kind, Kind::Llm | Kind::Webdav | Kind::Openalex)
    }

    pub fn secret(&self) -> Option<String> {
        Some(self.values.last()?.clone()).filter(|s| !s.is_empty() && self.has_secret())
    }

    pub fn masked(&self, field: usize) -> bool {
        self.has_secret() && field == self.values.len() - 1
    }

    /// The `[llm.<name>]`, `[locations.<name>]` or `[tables.<name>]` table, or `[fulltext]`'s proxies, checked by reading it back as yeda does.
    pub fn section(&self) -> Result<(toml::Table, Setup)> {
        if self.kind == Kind::Reference {
            return Ok((toml::Table::new(), Setup::Reference(self.reference()?)));
        }
        if self.name().is_empty() {
            bail!("give it a name");
        }
        let v: Vec<&str> = self.values.iter().map(|s| s.trim()).collect();
        let mut t = toml::Table::new();
        let setup = match self.kind {
            Kind::Llm => {
                let known = preset(v[1]);
                let api = match known {
                    Some((_, Provider::Anthropic, ..)) => "anthropic".to_string(),
                    Some(_) => "openai".to_string(),
                    None => v[1].to_lowercase(),
                };
                t.insert("provider".into(), api.clone().into());
                t.insert("model".into(), v[2].into());
                // The provider's own address is yeda's default, and leaving it out keeps the key required.
                let url = v[3].trim_end_matches('/');
                let builtin = PRESETS.iter().any(|q| q.0 == api && q.2 == url);
                if !url.is_empty() && !builtin {
                    t.insert("base_url".into(), url.into());
                }
                if v[2].is_empty() {
                    bail!("give the model's name");
                }
                let p = Profile::deserialize(toml::Value::Table(t.clone())).map_err(|_| anyhow::anyhow!("the provider is one of {}", PRESETS.map(|p| p.0).join(", ")))?;
                if p.needs_key() && self.secret().is_none() {
                    bail!("{} needs an API key", if p.provider == Provider::Anthropic { "Anthropic" } else { "OpenAI" });
                }
                Setup::Llm(p)
            }
            Kind::Webdav => {
                if !v[1].starts_with("https://") && !v[1].starts_with("http://") {
                    bail!("the URL starts with https://");
                }
                if v[2].is_empty() || self.secret().is_none() {
                    bail!("give the user name and password");
                }
                t.insert("url".into(), v[1].into());
                t.insert("user".into(), v[2].into());
                Setup::Webdav(Dav::deserialize(toml::Value::Table(t.clone()))?)
            }
            Kind::Openalex if self.secret().is_none() => bail!("paste the API key"),
            Kind::Openalex => Setup::Openalex,
            Kind::Reference => unreachable!(),
            Kind::Proxy => {
                let proxies: Vec<&str> = v[0].split_whitespace().collect();
                if let Some(p) = proxies.iter().find(|p| !p.starts_with("http") || !p.contains("{url}")) {
                    bail!("{p}: a proxy starts with https:// and has {{url}} where the paper's address goes");
                }
                if !proxies.is_empty() {
                    t.insert("proxies".into(), proxies.into());
                }
                Setup::Untested
            }
            Kind::Table => {
                if self.shown.is_empty() {
                    bail!("give it columns");
                }
                if let Some(c) = self.shown.iter().enumerate().find_map(|(i, c)| self.shown[..i].contains(c).then_some(c)) {
                    bail!("{c} is there twice");
                }
                let describe: toml::Table = self.shown.iter().zip(&v[2..]).filter(|(_, d)| !d.is_empty()).map(|(c, d)| (c.clone(), (*d).into())).collect();
                t.insert("columns".into(), self.shown.clone().into());
                if !describe.is_empty() {
                    t.insert("describe".into(), describe.into());
                }
                Setup::Untested
            }
            Kind::Search => {
                let (descending, field) = match v[2].strip_prefix('-') {
                    Some(f) => (true, f),
                    None => (false, v[2]),
                };
                let sort = match field {
                    "" => None,
                    f => Some(SortField::deserialize(toml::Value::String(f.into())).map_err(|_| anyhow::anyhow!("sort by key, title, author, year, status or type"))?),
                };
                Setup::Search(SavedSearch { query: v[1].into(), sort, descending: descending && sort.is_some() })
            }
        };
        Ok((t, setup))
    }
}

impl SetupForm {
    /// The reference the form describes, without a key.
    fn reference(&self) -> Result<Ref> {
        let v: Vec<&str> = self.values.iter().map(|s| s.trim()).collect();
        if v[1].is_empty() {
            bail!("give it a title");
        }
        let year = match v[3] {
            "" => None,
            y => Some(y.parse().map_err(|_| anyhow::anyhow!("the year is a number"))?),
        };
        let mut r = Ref {
            kind: if v[0].is_empty() { "misc".into() } else { v[0].to_lowercase() },
            title: v[1].into(),
            authors: v[2].split(';').map(str::trim).filter(|a| !a.is_empty()).map(String::from).collect(),
            year,
            doi: Some(v[4].to_lowercase()).filter(|d| !d.is_empty()),
            ..Default::default()
        };
        for (k, value) in EXTRAS.iter().zip(&v[5..]) {
            if !value.is_empty() {
                r.extra.insert(k.to_string(), (*value).into());
            }
        }
        Ok(r)
    }
}

/// The `:setup` list: what each row sets, how it is set now, and the command that changes it.
pub fn overview(lib: &Library) -> Vec<[String; 3]> {
    let row = |what: String, now: String, command: String| [what, now, command];
    let mut rows = Vec::new();
    for (name, p) in &lib.llm {
        let default = if lib.default_llm.as_deref() == Some(name) { " · default" } else { "" };
        rows.push(row(format!("LLM {name}"), format!("{} · {}{default}", p.model, p.url()), format!("setup llm {name}")));
    }
    rows.push(row("New LLM profile".into(), String::new(), "setup llm".into()));
    for (name, d) in &lib.remotes {
        rows.push(row(format!("WebDAV {name}"), format!("{}@{}", d.user, d.url), format!("setup webdav {name}")));
    }
    rows.push(row("New WebDAV location".into(), String::new(), "setup webdav".into()));
    rows.push(row("OpenAlex API key".into(), "optional; :secrets shows whether it is set".into(), "setup openalex".into()));
    let proxies = if lib.fulltext.proxies.is_empty() { "none".into() } else { lib.fulltext.proxies.join(" ") };
    rows.push(row("Library proxies".into(), proxies, "setup proxy".into()));
    for (name, t) in &lib.tables {
        rows.push(row(format!("Table {name}"), t.columns.join(", "), format!("setup table {name}")));
    }
    rows.push(row("New table".into(), String::new(), "setup table".into()));
    rows.push(row("yeda.toml".into(), "everything else in the library's config".into(), "config library".into()));
    rows.push(row("config.toml".into(), "this device's config".into(), "config device".into()));
    rows
}

/// Tests the connection; blocks.
pub fn test(setup: &Setup, secret: Option<&str>) -> Result<()> {
    match setup {
        Setup::Llm(p) => yeda_net::llm::ping(p, secret),
        Setup::Webdav(d) => yeda_net::webdav::login(d, secret.unwrap_or_default()),
        Setup::Search(_) | Setup::Reference(_) | Setup::Untested => Ok(()),
        Setup::Openalex => yeda_net::openalex::Client::new(secret.map(String::from)).one_by_doi("10.1038/nature16961").map(drop),
    }
}

/// The secret-store account the secret is kept under.
pub fn account(setup: &Setup) -> String {
    match setup {
        Setup::Llm(p) => yeda_secrets::llm_account(p.url()),
        Setup::Webdav(d) => yeda_secrets::webdav_account(&d.user, &d.url),
        Setup::Search(_) | Setup::Reference(_) | Setup::Untested => String::new(),
        Setup::Openalex => yeda_secrets::OPENALEX.into(),
    }
}

/// As `[searches]` entries spell it.
fn sort_name(f: SortField) -> &'static str {
    match f {
        SortField::Author => "author",
        SortField::Status => "status",
        f => f.name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_checks_sections() {
        let lib = Library::parse(Some("[llm.local]\nprovider = \"openai\"\nmodel = \"qwen\"\nbase_url = \"http://localhost:8080/v1\"\n"), None, [], None).unwrap();
        let f = SetupForm::new(Kind::Llm, "local", &lib);
        assert_eq!(f.values, ["local", "openai", "qwen", "http://localhost:8080/v1", ""]);
        let mut g = SetupForm::new(Kind::Llm, "new", &lib);
        assert_eq!(g.values[1..4], ["anthropic", "", "https://api.anthropic.com"]);
        g.cycle_provider(false);
        g.cycle_provider(false);
        g.values[2] = "gemini-x".into();
        g.values[4] = "k".into();
        assert_eq!(g.values[3], "https://generativelanguage.googleapis.com/v1beta/openai");
        let (t, s) = g.section().unwrap();
        assert_eq!(t["provider"].as_str(), Some("openai"));
        assert_eq!(account(&s), "llm:https://generativelanguage.googleapis.com/v1beta/openai");
        g.cycle_provider(true);
        g.cycle_provider(true);
        assert!(!g.section().unwrap().0.contains_key("base_url"));
        let (t, _) = f.section().unwrap();
        assert_eq!(t.get("base_url").and_then(|v| v.as_str()), Some("http://localhost:8080/v1"));

        let mut f = SetupForm::new(Kind::Llm, "claude", &lib);
        f.values[2] = "claude-opus-5-5".into();
        assert!(f.section().unwrap_err().to_string().contains("API key"));
        f.values[1] = "cohere".into();
        assert!(f.section().is_err());

        let mut f = SetupForm::new(Kind::Webdav, "dav", &lib);
        f.values[1..].clone_from_slice(&["https://dav.example.com/pdfs".into(), "me".into(), "pw".into()]);
        let (t, s) = f.section().unwrap();
        assert!(!t.contains_key("password"));
        assert_eq!(account(&s), "webdav:me@https://dav.example.com/pdfs");

        let lib = Library::parse(Some("[searches]\nnew = { query = \"tag:x\", sort = \"author\", descending = true }\n"), None, [], None).unwrap();
        let copy = SetupForm::duplicate("new", &lib);
        assert_eq!((copy.values.as_slice(), copy.old.as_deref()), (["new copy", "tag:x", "-author"].map(String::from).as_slice(), None));
        let mut f = SetupForm::new(Kind::Search, "new", &lib);
        assert_eq!(f.values, ["new", "tag:x", "-author"]);
        f.values[2] = "year".into();
        let Ok((_, Setup::Search(s))) = f.section() else { panic!() };
        assert_eq!((s.sort, s.descending), (Some(SortField::Year), false));
        f.values[2] = "-pages".into();
        assert!(f.section().is_err());
    }

    #[test]
    fn builds_tables_and_proxies() {
        let lib = Library::parse(Some("[tables.lit]\ncolumns = [\"a\", \"b\"]\nexamples = 2\n[tables.lit.describe]\nb = \"the b\"\n"), None, [], None).unwrap();
        let mut f = SetupForm::new(Kind::Table, "lit", &lib);
        assert_eq!(f.values, ["lit", "a, b", "", "the b"]);
        f.values[1] = "b, c".into();
        f.sync_columns();
        assert_eq!(f.values[2..], ["the b", ""]);
        f.values[3] = "the c".into();
        f.values[1] = "a, c".into();
        f.sync_columns();
        assert_eq!(f.values[2..], ["", "the c"]);
        let (t, _) = f.section().unwrap();
        assert_eq!(t.to_string(), "columns = [\"a\", \"c\"]\n\n[describe]\nc = \"the c\"\n");
        f.values[1] = "a, a".into();
        f.sync_columns();
        assert!(f.section().unwrap_err().to_string().contains("twice"));

        let mut f = SetupForm::new(Kind::Proxy, "", &lib);
        assert!(f.section().unwrap().0.is_empty());
        f.values[0] = "https://p.example.edu/login?url={url} https://q.example.edu/".into();
        assert!(f.section().unwrap_err().to_string().starts_with("https://q."));
        f.values[0] = "https://p.example.edu/login?url={url}".into();
        assert_eq!(f.section().unwrap().0["proxies"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn builds_references() {
        let lib = Library::parse(None, None, [], None).unwrap();
        let mut f = SetupForm::new(Kind::Reference, "", &lib);
        assert_eq!((f.values[0].as_str(), f.old.as_deref(), f.secret()), ("article", None, None));
        assert!(f.section().is_err());
        f.values[..5].clone_from_slice(&["Report".into(), " Swarms ".into(), "Doe, J.; ; Roe, R.".into(), "20x".into(), "".into()]);
        assert!(f.section().unwrap_err().to_string().contains("year"));
        f.values[3] = "2021".into();
        f.values[5] = "https://example.com".into();
        let Ok((_, Setup::Reference(r))) = f.section() else { panic!() };
        assert_eq!((r.kind.as_str(), r.title.as_str(), r.authors.len(), r.year, r.doi), ("report", "Swarms", 2, Some(2021), None));
        assert_eq!(r.extra.keys().collect::<Vec<_>>(), ["url"]);
        assert_eq!(r.status, "New");
    }
}
