use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use yeda_core::browse::SortField;
use yeda_core::library::{Dav, Library, Ref, Remote, SavedSearch};
use yeda_core::llm::{Auth, Bound, Profile, Protocol, Provider};
use yeda_core::web_search::Provider as WebProvider;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// A `[providers.<name>]` entry: where to connect, and how.
    Provider,
    Llm,
    /// A `[locations.<name>]` table for PDFs: a local folder, a WebDAV server or Dropbox.
    Storage,
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
    /// The optional API key of a web search provider.
    Keenable,
    Exa,
    Parallel,
    Firecrawl,
    /// The URL of a SearXNG instance.
    Searxng,
}

impl Kind {
    /// The web search provider this form sets up.
    pub fn web_search_provider(self) -> Option<WebProvider> {
        match self {
            Kind::Keenable => Some(WebProvider::Keenable),
            Kind::Exa => Some(WebProvider::Exa),
            Kind::Parallel => Some(WebProvider::Parallel),
            Kind::Firecrawl => Some(WebProvider::Firecrawl),
            Kind::Searxng => Some(WebProvider::Searxng),
            _ => None,
        }
    }
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
    /// A Codex sign-in waiting for the browser: the link the user was given.
    pub sign_in_url: Option<String>,
    /// A Dropbox sign-in waiting for its code: the link the user was given, and the PKCE verifier
    /// the code is exchanged with.
    pub code_link: Option<(String, String)>,
    /// The name and URL of the provider being edited, as they were when the form opened.
    pub origin: Option<(String, String)>,
    /// Waiting for y/n: the provider's key would be sent to a new URL.
    pub confirm_move: bool,
    pub moving_confirmed: bool,
    /// A table's columns that have description fields now, and every description typed, by column.
    shown: Vec<String>,
    described: BTreeMap<String, String>,
}

/// A `:setup` field whose values are known, but which still accepts a typed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Presets,
    Protocols,
    Auth,
    BoundProviders,
    StorageKinds,
    /// Filled from the network, not from the library.
    Models,
    Sort,
    RefKind,
}

/// A predefined provider: what its form starts with.
pub struct Preset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub protocol: &'static str,
    pub auth: &'static str,
}

/// Providers the form knows. The last allows any OpenAI-compatible server at a typed URL.
pub const PRESETS: [Preset; 7] = [
    Preset { name: "codex", base_url: "https://api.openai.com/v1", protocol: "openai-responses", auth: "oauth" },
    Preset { name: "claude", base_url: "https://api.anthropic.com", protocol: "anthropic", auth: "oauth" },
    Preset { name: "mistral", base_url: "https://api.mistral.ai/v1", protocol: "mistral", auth: "api-key" },
    Preset { name: "openrouter", base_url: "https://openrouter.ai/api/v1", protocol: "openrouter", auth: "api-key" },
    Preset { name: "opencode-go", base_url: "https://opencode.ai/inference/go/openai/v1", protocol: "opencode-go", auth: "api-key" },
    Preset { name: "gemini", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", protocol: "openai", auth: "api-key" },
    Preset { name: "openai-compatible", base_url: "", protocol: "openai", auth: "api-key" },
];

fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name.trim().to_lowercase())
}

fn protocol_of(name: &str) -> Protocol {
    match name {
        "anthropic" => Protocol::Anthropic,
        "mistral" => Protocol::Mistral,
        "openrouter" => Protocol::Openrouter,
        "opencode-go" => Protocol::OpencodeGo,
        "openai-responses" => Protocol::OpenaiResponses,
        _ => Protocol::Openai,
    }
}

fn auth_of(name: &str) -> Auth {
    match name {
        "oauth" => Auth::Oauth,
        "none" => Auth::None,
        _ => Auth::ApiKey,
    }
}

/// The reference form's last fields, kept in `Ref::extra`.
pub const EXTRAS: [&str; 4] = ["url", "journal", "publisher", "abstract"];

const ALIAS_HINT: &str = "optional; names the environment variable for this secret when several share a server: YEDA_…_ALIAS_<ALIAS>";
const NAME_HINT: &str = "used as pdf.location and in :attach";
const SIGN_IN_HINT: &str = "press Enter on an empty field to sign in in the browser; a saved sign-in is reused, type signin to sign in again";
const CHOICE_HINT: &str = "all options shown · type to filter · ↓/↑ pick · Enter use · Esc restores · free text works";

/// What the form saves, once its connection works.
#[derive(Clone, Debug)]
pub enum Setup {
    Provider(String, Provider),
    Llm(String, Profile),
    Webdav(Dav),
    /// A Dropbox location by name: its secret is a sign-in, which the form makes.
    Cloud(String, Remote),
    /// A local folder, kept in the device config because its path belongs to this device.
    Local(String, String),
    Search(SavedSearch),
    Openalex,
    Reference(Ref),
    /// A web search provider's key, tested with one real search, which spends quota.
    ApiKey(WebProvider),
    /// A SearXNG instance URL, tested with one JSON search.
    Searxng(String),
    /// Saved as it is, with nothing to test.
    Untested,
}

impl SetupForm {
    /// Filled in from the location, search or reference `name`, if there is one. Provider and LLM
    /// forms start empty: the preset or provider the user picks fills the rest of the form.
    pub fn new(kind: Kind, name: &str, lib: &Library) -> SetupForm {
        let values = match kind {
            Kind::Provider => match lib.providers.get(name) {
                Some(p) => {
                    let preset = PRESETS.iter().find(|q| q.base_url == p.url()).map_or("", |q| q.name);
                    [name, preset, p.url(), p.protocol.name(), p.auth.name(), p.alias.as_deref().unwrap_or_default(), ""].map(String::from).to_vec()
                }
                None => [name, "", "", "", "", "", ""].map(String::from).to_vec(),
            },
            Kind::Llm => match lib.llm.get(name) {
                Some(p) => vec![name.into(), p.provider.clone(), p.model.clone()],
                None => vec![name.into(), String::new(), String::new()],
            },
            Kind::Storage => {
                let row = |cells: &[&str]| cells.iter().map(|c| c.to_string()).collect::<Vec<_>>();
                match (lib.remotes.get(name), lib.locations.get(name)) {
                    (Some(Remote::Webdav(d)), _) => row(&[name, "webdav", &d.url, &d.user, d.alias.as_deref().unwrap_or_default(), ""]),
                    (Some(Remote::Dropbox(c)), _) => row(&[name, "dropbox", &c.folder, ""]),
                    (None, Some(path)) => row(&[name, "local", &path.to_string_lossy()]),
                    (None, None) => row(&[name, ""]),
                }
            }
            Kind::Search => {
                let s = lib.searches.get(name);
                let sort = s.and_then(|s| Some(format!("{}{}", if s.descending { "-" } else { "" }, sort_name(s.sort?))));
                vec![name.into(), s.map(|s| s.query.clone()).unwrap_or_default(), sort.unwrap_or_default()]
            }
            Kind::Openalex => vec![String::new()],
            Kind::Keenable | Kind::Exa | Kind::Parallel | Kind::Firecrawl => vec![String::new()],
            Kind::Searxng => vec![lib.web_search.searxng_url.clone().unwrap_or_default(), String::new(), String::new()],
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
            Kind::Provider | Kind::Llm if !name.is_empty() => 1,
            Kind::Provider | Kind::Llm | Kind::Openalex | Kind::Proxy | Kind::Searxng => 0,
            Kind::Keenable | Kind::Exa | Kind::Parallel | Kind::Firecrawl => 0,
            _ if name.is_empty() => 0,
            _ => 1,
        };
        let old = match kind {
            Kind::Search => Some(name.to_string()),
            Kind::Reference => Some(name.to_string()).filter(|n| !n.is_empty()),
            _ => None,
        };
        let described = if kind == Kind::Table { lib.tables.get(name).map(|t| t.describe.clone()).unwrap_or_default() } else { BTreeMap::new() };
        let origin = lib.providers.get(name).filter(|_| kind == Kind::Provider).map(|p| (name.to_string(), p.url().to_string()));
        let mut form = SetupForm { kind, values, sel, testing: false, old, sign_in_url: None, code_link: None, origin, confirm_move: false, moving_confirmed: false, shown: Vec::new(), described };
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

    /// The Kind field was edited: the form gets that kind's fields, and keeps the name and the
    /// path, URL or folder, which every kind has.
    pub fn storage_kind_changed(&mut self) {
        let wanted = match self.values[1].trim().to_lowercase().as_str() {
            "local" => 3,
            "webdav" => 6,
            "dropbox" => 4,
            _ => 2,
        };
        if self.values.len() != wanted {
            self.values.truncate(wanted.min(3));
            self.values.resize(wanted, String::new());
            self.code_link = None;
        }
    }

    /// A known preset replaces the base URL, protocol and auth, unless a URL was typed that no
    /// preset has.
    pub fn provider_changed(&mut self) {
        let url = self.values[2].trim();
        if let Some(p) = preset(&self.values[1])
            && (url.is_empty() || PRESETS.iter().any(|q| q.base_url == url))
        {
            self.values[2] = p.base_url.into();
            self.values[3] = p.protocol.into();
            self.values[4] = p.auth.into();
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
            Kind::Provider => {
                let protocol = protocol_of(&self.values[3]);
                let auth = auth_of(&self.values[4]);
                let token = match (auth, protocol) {
                    (Auth::None, _) => "none: this server takes no key",
                    (Auth::Oauth, Protocol::OpenaiResponses) => "press Enter and open the link yeda shows",
                    (Auth::Oauth, _) => "paste a token from `claude setup-token`",
                    _ if self.origin.is_some() => "blank keeps the stored key, moved to the new name or URL if either changed; type one to replace it",
                    _ => "API key",
                };
                &[
                    ("Name", "profiles name it: [llm.<name>] provider = \"…\""),
                    ("Preset", CHOICE_HINT),
                    ("Base URL", "filled in by the preset; any https server for openai-compatible"),
                    ("Protocol", CHOICE_HINT),
                    ("Auth", CHOICE_HINT),
                    ("Alias", ALIAS_HINT),
                    ("Key / token", token),
                ]
            }
            Kind::Llm => &[
                ("Name", "used as --llm <name>"),
                ("Provider", CHOICE_HINT),
                ("Model", CHOICE_HINT),
            ],
            Kind::Storage => match self.values[1].trim().to_lowercase().as_str() {
                "local" => &[("Name", NAME_HINT), ("Kind", CHOICE_HINT), ("Path", "this device's folder for the PDFs, e.g. ~/papers; made if missing")],
                "webdav" => &[
                    ("Name", NAME_HINT),
                    ("Kind", CHOICE_HINT),
                    ("URL", "the folder PDFs go in, e.g. https://dav.example.com/pdfs/"),
                    ("User", ""),
                    ("Alias", ALIAS_HINT),
                    ("Password", "kept in the secret store"),
                ],
                "dropbox" => &[
                    ("Name", NAME_HINT),
                    ("Kind", CHOICE_HINT),
                    ("Folder", "inside yeda's Dropbox app folder, e.g. pdfs; empty is the app folder itself"),
                    ("Code", if self.code_link.is_some() { "paste the code Dropbox shows, then press Enter" } else { SIGN_IN_HINT }),
                ],
                _ => &[("Name", NAME_HINT), ("Kind", CHOICE_HINT)],
            },
            Kind::Openalex => &[("API key", "free at openalex.org/settings/api; kept in the secret store")],
            Kind::Keenable => &[("API key", "optional; without one, search uses the shared public tier (1,000 requests/hour per IP); the test runs one real search and uses quota")],
            Kind::Exa => &[("API key", "optional; without one, search uses the shared free tier, which needs a direct IPv4 connection; the test runs one real search and uses quota")],
            Kind::Parallel => &[("API key", "optional; without one, search uses the shared free tier; keyed use is billed by Parallel; the test runs one real search and uses quota")],
            Kind::Firecrawl => &[("API key", "optional; without one, search uses the shared free tier with lower limits; the test runs one real search and uses quota")],
            Kind::Searxng => &[
                ("Instance URL", "e.g. https://searx.example.org; https only; the instance must have JSON output enabled (search.formats in settings.yml)"),
                ("User", "optional, for an instance behind basic auth; kept with the password in the secret store; blank keeps what is stored"),
                ("Password", "kept in the secret store"),
            ],
            Kind::Reference => &[
                ("Type", CHOICE_HINT),
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
                ("Sort", CHOICE_HINT),
            ],
        };
        fields.iter().map(|(label, hint)| (label.to_string(), *hint)).collect()
    }

    /// Whether this field offers known values, which it may still ignore in favour of typed text.
    pub fn choice(&self, field: usize) -> Option<Choice> {
        match (self.kind, field) {
            (Kind::Provider, 1) => Some(Choice::Presets),
            (Kind::Provider, 3) => Some(Choice::Protocols),
            (Kind::Provider, 4) => Some(Choice::Auth),
            (Kind::Llm, 1) => Some(Choice::BoundProviders),
            (Kind::Storage, 1) => Some(Choice::StorageKinds),
            (Kind::Llm, 2) => Some(Choice::Models),
            (Kind::Search, 2) => Some(Choice::Sort),
            (Kind::Reference, 0) => Some(Choice::RefKind),
            _ => None,
        }
    }

    pub fn name(&self) -> &str {
        match self.kind {
            Kind::Openalex => "openalex",
            Kind::Proxy => "proxy",
            Kind::Keenable => "keenable",
            Kind::Exa => "exa",
            Kind::Parallel => "parallel",
            Kind::Firecrawl => "firecrawl",
            Kind::Searxng => "searxng",
            _ => self.values[0].trim(),
        }
    }

    fn has_secret(&self) -> bool {
        if self.kind == Kind::Storage {
            return matches!(self.values[1].trim().to_lowercase().as_str(), "webdav" | "dropbox");
        }
        matches!(self.kind, Kind::Provider | Kind::Openalex | Kind::Keenable | Kind::Exa | Kind::Parallel | Kind::Firecrawl | Kind::Searxng)
    }

    /// What goes in the secret store: the last field, or `user:password` for SearXNG.
    pub fn secret(&self) -> Option<String> {
        let last = self.values.last()?.trim();
        if self.kind == Kind::Searxng {
            let user = self.values[1].trim();
            return (!user.is_empty() && !last.is_empty()).then(|| format!("{user}:{last}"));
        }
        Some(self.values.last()?.clone()).filter(|s| !s.is_empty() && self.has_secret())
    }

    pub fn masked(&self, field: usize) -> bool {
        self.has_secret() && field == self.values.len() - 1
    }

    /// The `[providers.<name>]`, `[llm.<name>]`, `[locations.<name>]` or `[tables.<name>]` table, or
    /// `[fulltext]`'s proxies, checked by reading it back as yeda does.
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
            Kind::Provider => {
                t.insert("base_url".into(), v[2].trim_end_matches('/').into());
                t.insert("protocol".into(), v[3].to_lowercase().into());
                t.insert("auth".into(), v[4].to_lowercase().into());
                if !v[5].is_empty() {
                    t.insert("alias".into(), v[5].into());
                }
                let p = Provider::deserialize(toml::Value::Table(t.clone())).map_err(|_| {
                    anyhow::anyhow!("the protocol is one of anthropic, openai, mistral, openrouter, opencode-go, openai-responses, and the auth api-key, oauth or none")
                })?;
                p.check()?;
                if let Some(alias) = &p.alias {
                    yeda_core::check_alias(alias)?;
                }
                // Codex signs in when the field is empty; every other credential has to be pasted,
                // since its key is bound to this URL and a new one has none yet.
                // An edited provider has its stored key moved along, which the app looks up.
                let editing = self.origin.is_some();
                if p.auth == Auth::ApiKey && self.secret().is_none() && !editing {
                    bail!("{} needs an API key", v[0]);
                }
                if p.auth == Auth::Oauth && p.protocol == Protocol::Anthropic && self.secret().is_none() && !editing {
                    bail!("paste the token from `claude setup-token`");
                }
                Setup::Provider(v[0].to_string(), p)
            }
            Kind::Llm => {
                t.insert("provider".into(), v[1].into());
                t.insert("model".into(), v[2].into());
                let p = Profile { provider: v[1].to_string(), model: v[2].to_string() };
                if p.provider.is_empty() {
                    bail!("pick a provider; set one up with :setup provider");
                }
                if p.model.is_empty() {
                    bail!("give the model's name");
                }
                Setup::Llm(v[0].to_string(), p)
            }
            Kind::Storage => {
                let kind = v[1].to_lowercase();
                if kind == "local" {
                    if v[2].is_empty() {
                        bail!("give the folder's path");
                    }
                    return Ok((toml::Table::new(), Setup::Local(v[0].to_string(), v[2].to_string())));
                }
                t.insert("kind".into(), kind.clone().into());
                match kind.as_str() {
                    "webdav" => {
                        // The password goes with every request.
                        yeda_core::require_https(v[2])?;
                        if v[3].is_empty() || self.secret().is_none() {
                            bail!("give the user name and password");
                        }
                        t.insert("url".into(), v[2].into());
                        t.insert("user".into(), v[3].into());
                        if !v[4].is_empty() {
                            yeda_core::check_alias(v[4])?;
                            t.insert("alias".into(), v[4].into());
                        }
                    }
                    "dropbox" => {
                        t.insert("folder".into(), v[2].trim_matches('/').into());
                    }
                    _ => bail!("the kind is local, webdav or dropbox"),
                }
                match Remote::deserialize(toml::Value::Table(t.clone()))? {
                    Remote::Webdav(d) => Setup::Webdav(d),
                    remote => Setup::Cloud(v[0].to_string(), remote),
                }
            }
            Kind::Openalex if self.secret().is_none() => bail!("paste the API key"),
            Kind::Openalex => Setup::Openalex,
            Kind::Keenable | Kind::Exa | Kind::Parallel | Kind::Firecrawl if self.secret().is_none() => bail!("paste the API key"),
            Kind::Keenable | Kind::Exa | Kind::Parallel | Kind::Firecrawl => Setup::ApiKey(self.kind.web_search_provider().unwrap()),
            Kind::Searxng => {
                yeda_core::web_search::check_searxng_url(v[0])?;
                if v[1].is_empty() != v[2].is_empty() {
                    bail!("give the user name and the password, or neither");
                }
                if v[1].contains(':') {
                    bail!("a basic-auth user name has no ':'");
                }
                t.insert("searxng_url".into(), v[0].trim_end_matches('/').into());
                Setup::Searxng(v[0].trim_end_matches('/').to_string())
            }
            Kind::Reference => unreachable!(),
            Kind::Proxy => {
                let proxies: Vec<&str> = v[0].split_whitespace().collect();
                for p in &proxies {
                    // You log in to the proxy in the fetch window: that password must not go in the clear either.
                    yeda_core::require_https(p)?;
                    if !p.contains("{url}") {
                        bail!("{p}: a proxy has {{url}} where the paper's address goes");
                    }
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

/// Values the choice fields offer, as `(label, value)`; free text is always allowed too.
pub fn choices(choice: Choice, lib: &Library) -> Vec<(String, String)> {
    let pairs: &[(&str, &str)] = match choice {
        Choice::Presets => return PRESETS.iter().map(|p| (p.name.to_string(), p.name.to_string())).collect(),
        Choice::BoundProviders => return lib.providers.keys().map(|n| (n.clone(), n.clone())).collect(),
        Choice::Models => return Vec::new(),
        Choice::Protocols => &[
            ("anthropic", "anthropic"),
            ("openai", "openai"),
            ("mistral", "mistral"),
            ("openrouter", "openrouter"),
            ("opencode-go", "opencode-go"),
            ("openai-responses", "openai-responses"),
        ],
        Choice::Auth => &[("api-key", "api-key"), ("oauth", "oauth"), ("none", "none")],
        Choice::StorageKinds => &[("local: a folder on this device", "local"), ("webdav: any WebDAV server", "webdav"), ("dropbox", "dropbox")],
        Choice::Sort => &[
            ("key", "key"),
            ("title", "title"),
            ("author", "author"),
            ("year", "year"),
            ("status", "status"),
            ("type", "type"),
            ("key descending", "-key"),
            ("title descending", "-title"),
            ("author descending", "-author"),
            ("year descending", "-year"),
            ("status descending", "-status"),
            ("type descending", "-type"),
        ],
        Choice::RefKind => &[
            ("article", "article"),
            ("inproceedings", "inproceedings"),
            ("incollection", "incollection"),
            ("book", "book"),
            ("preprint", "preprint"),
            ("report", "report"),
            ("thesis", "thesis"),
            ("misc", "misc"),
        ],
    };
    pairs.iter().map(|(label, value)| (label.to_string(), value.to_string())).collect()
}

/// Whether `:setup` lists the existing entries of this kind to pick from, before the form.
pub fn listed(kind: Kind) -> bool {
    matches!(kind, Kind::Provider | Kind::Llm | Kind::Storage | Kind::Table)
}

/// What an entry of a listed kind is called in the picker.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Provider => "provider",
        Kind::Llm => "LLM profile",
        Kind::Storage => "storage location",
        Kind::Table => "table",
        _ => "",
    }
}

/// The existing entries of a listed kind, as name and how it is set now.
pub fn entries(kind: Kind, lib: &Library) -> Vec<(String, String)> {
    let default_mark = |is_default: bool| if is_default { " · default" } else { "" };
    match kind {
        Kind::Provider => lib.providers.iter().map(|(n, p)| (n.clone(), format!("{} · {} · {}", p.protocol.name(), p.auth.name(), p.url()))).collect(),
        Kind::Llm => lib.llm.iter().map(|(n, p)| (n.clone(), format!("{} · {}{}", p.provider, p.model, default_mark(lib.default_llm.as_deref() == Some(n))))).collect(),
        Kind::Storage => {
            let local = lib.locations.iter().map(|(n, path)| (n.clone(), format!("local {}", path.display())));
            let remote = lib.remotes.iter().map(|(n, r)| (n.clone(), r.label()));
            let all = local.chain(remote).collect::<BTreeMap<_, _>>();
            all.into_iter().map(|(n, what)| { let mark = default_mark(lib.fulltext.location.as_deref() == Some(n.as_str())); (n, format!("{what}{mark}")) }).collect()
        }
        Kind::Table => lib.tables.iter().map(|(n, t)| (n.clone(), t.columns.join(", "))).collect(),
        _ => Vec::new(),
    }
}

/// The `:setup` list: what each row sets, how it is set now, and the command that changes it.
pub fn overview(lib: &Library) -> Vec<[String; 3]> {
    let row = |what: String, now: String, command: String| [what, now, command];
    let mut rows = Vec::new();
    let listed = |kind| {
        let entries = entries(kind, lib);
        if entries.is_empty() { "(none)".to_string() } else { entries.into_iter().map(|(name, _)| name).collect::<Vec<_>>().join(" ") }
    };
    rows.push(row("LLM providers".into(), String::new(), "setup provider".into()));
    rows.push(row("LLM profiles".into(), listed(Kind::Llm), "setup llm".into()));
    rows.push(row("Storage".into(), listed(Kind::Storage), "setup storage".into()));
    rows.push(row("OpenAlex API key".into(), "optional; :secrets shows whether it is set".into(), "setup openalex".into()));
    let names = |list: &[yeda_core::web_search::Provider]| if list.is_empty() { "(none)".to_string() } else { list.iter().map(|p| p.name()).collect::<Vec<_>>().join(" ") };
    let web = format!("always {} · fallback {}", names(&lib.web_search.always), names(&lib.web_search.fallback));
    rows.push(row("Web search".into(), web, "setup web-search".into()));
    let proxies = if lib.fulltext.proxies.is_empty() { "none".into() } else { lib.fulltext.proxies.join(" ") };
    rows.push(row("Library proxies".into(), proxies, "setup proxy".into()));
    rows.push(row("Tables".into(), listed(Kind::Table), "setup table".into()));
    rows.push(row("yeda.toml".into(), "everything else in the library's config".into(), "config library".into()));
    rows.push(row("config.toml".into(), "this device's config".into(), "config device".into()));
    rows
}

/// The profile bound to the provider it names.
pub fn bind(providers: &BTreeMap<String, Provider>, name: &str, p: &Profile) -> Result<Bound> {
    let provider = providers.get(&p.provider).with_context(|| format!("no provider named '{}'; set it up with :setup provider", p.provider))?;
    Ok(Bound::new(name, p, &p.provider, provider))
}

/// A profile bound to its provider, plus the credential a connection test sends: whatever the
/// provider's account holds, or nothing when the provider takes no key.
pub fn profile_credential(
    providers: &BTreeMap<String, Provider>,
    name: &str,
    p: &Profile,
    lookup: impl Fn(&str) -> Result<Option<String>>,
) -> Result<(Bound, Option<String>)> {
    let bound = bind(providers, name, p)?;
    let key = lookup(&yeda_secrets::provider_account(&bound.provider, bound.url()))?;
    if bound.needs_key() && key.is_none() {
        bail!("provider '{}' has no credential yet; set it with :setup provider", bound.provider);
    }
    Ok((bound, key))
}

/// Tests the connection; blocks. A provider form has no model, so its profile tests it.
pub fn test(providers: &BTreeMap<String, Provider>, setup: &Setup, secret: Option<&str>) -> Result<()> {
    match setup {
        Setup::Provider(..) => Ok(()),
        Setup::Llm(name, p) => {
            yeda_net::llm::ping(&bind(providers, name, p)?, secret)
        }
        Setup::Webdav(d) => yeda_net::webdav::login(d, secret.unwrap_or_default()),
        Setup::Cloud(_, remote) => yeda_net::remote::login(remote, secret.unwrap_or_default()),
        Setup::Local(..) => Ok(()),
        Setup::Search(_) | Setup::Reference(_) | Setup::Untested => Ok(()),
        Setup::Openalex => yeda_net::openalex::Client::new(secret.map(String::from)).one_by_doi("10.1038/nature16961").map(drop),
        Setup::ApiKey(provider) => yeda_net::web_search::search(&yeda_net::web_search::Search {
            provider: *provider,
            query: "yeda api key test",
            limit: 3,
            key: Some(secret.unwrap_or_default()),
            searxng_url: None,
        })
        .map(drop)
        .map_err(|e| anyhow::anyhow!(e)),
        Setup::Searxng(url) => yeda_net::web_search::search(&yeda_net::web_search::Search {
            provider: WebProvider::Searxng,
            query: "yeda searxng test",
            limit: 3,
            key: secret,
            searxng_url: Some(url),
        })
        .map(drop)
        .map_err(|e| anyhow::anyhow!(e)),
    }
}

/// True when stored Codex OAuth credentials expire soon enough to be worth renewing now.
pub fn codex_refresh_due(key: &str) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_secs() as i64).unwrap_or_default();
    yeda_core::llm::CodexCredential::parse(key).is_ok_and(|c| c.needs_refresh(10 * 60, now))
}

/// The secret-store account the secret is kept under.
pub fn account(setup: &Setup) -> String {
    match setup {
        Setup::Provider(name, p) => yeda_secrets::provider_account(name, p.url()),
        Setup::Webdav(d) => yeda_secrets::webdav_account(&d.user, &d.url),
        Setup::Cloud(name, remote) => yeda_secrets::remote_account(name, remote),
        Setup::Local(..) => String::new(),
        Setup::Llm(..) | Setup::Search(_) | Setup::Reference(_) | Setup::Untested => String::new(),
        Setup::Openalex => yeda_secrets::OPENALEX.into(),
        Setup::ApiKey(provider) => crate::websearch::account(*provider).into(),
        Setup::Searxng(url) => yeda_secrets::searxng_account(url),
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
    fn choice_fields_offer_known_values() {
        let cfg = "[providers.claude]\nbase_url = \"https://api.anthropic.com\"\nprotocol = \"anthropic\"\nauth = \"oauth\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let llm = SetupForm::new(Kind::Llm, "new", &lib);
        assert_eq!(llm.choice(1), Some(Choice::BoundProviders));
        assert_eq!(llm.choice(2), Some(Choice::Models));
        assert_eq!(choices(Choice::BoundProviders, &lib), [("claude".into(), "claude".into())]);
        assert!(choices(Choice::Models, &lib).is_empty());
        let provider = SetupForm::new(Kind::Provider, "", &lib);
        assert_eq!(provider.choice(1), Some(Choice::Presets));
        assert_eq!(provider.choice(5), None);
        let search = SetupForm::new(Kind::Search, "", &lib);
        assert_eq!(search.choice(2), Some(Choice::Sort));
        assert!(choices(Choice::Sort, &lib).contains(&("year descending".into(), "-year".into())));
        let reference = SetupForm::new(Kind::Reference, "", &lib);
        assert_eq!(reference.choice(0), Some(Choice::RefKind));
    }

    #[test]
    fn builds_provider_keys_and_searxng() {
        let lib = Library::parse(None, None, [], None).unwrap();
        let mut key = SetupForm::new(Kind::Exa, "", &lib);
        assert!(key.section().unwrap_err().to_string().contains("paste the API key"));
        key.values[0] = "k".into();
        let (t, setup) = key.section().unwrap();
        assert!(t.is_empty());
        assert!(matches!(setup, Setup::ApiKey(WebProvider::Exa)));
        assert_eq!(account(&setup), yeda_secrets::EXA);

        let mut searx = SetupForm::new(Kind::Searxng, "", &lib);
        searx.values[0] = "http://searx.example.org".into();
        assert!(searx.section().unwrap_err().to_string().contains("https://"));
        searx.values[0] = "https://searx.example.org/".into();
        let (t, setup) = searx.section().unwrap();
        assert_eq!(t["searxng_url"].as_str(), Some("https://searx.example.org"));
        assert!(matches!(setup, Setup::Searxng(ref url) if url == "https://searx.example.org"));
        assert_eq!(searx.secret(), None);
        assert_eq!(account(&setup), "searxng:https://searx.example.org");

        searx.values[1] = "me".into();
        assert!(searx.section().unwrap_err().to_string().contains("or neither"));
        searx.values[2] = "pw".into();
        assert_eq!(searx.secret().as_deref(), Some("me:pw"));
        let (t, _) = searx.section().unwrap();
        assert!(!t.values().any(|v| v.as_str().is_some_and(|s| s.contains("pw"))));
        searx.values[1] = "a:b".into();
        assert!(searx.section().unwrap_err().to_string().contains("':'"));
        assert!(searx.masked(2) && !searx.masked(1));
    }

    #[test]
    fn builds_providers_from_presets() {
        let lib = Library::parse(None, None, [], None).unwrap();
        let mut f = SetupForm::new(Kind::Provider, "", &lib);
        assert_eq!(f.values, [""; 7], "a new provider form starts empty");
        f.values[0] = "codex".into();
        f.values[1] = "codex".into();
        f.provider_changed();
        assert_eq!(f.values, ["codex", "codex", "https://api.openai.com/v1", "openai-responses", "oauth", "", ""]);
        // Codex signs in with an empty field, so the section builds without a credential.
        let (t, s) = f.section().unwrap();
        assert_eq!(t.to_string(), "base_url = \"https://api.openai.com/v1\"\nprotocol = \"openai-responses\"\nauth = \"oauth\"\n");
        assert_eq!(account(&s), "provider:codex:https://api.openai.com/v1");

        f.values[1] = "claude".into();
        f.provider_changed();
        assert_eq!(f.values[2..5], ["https://api.anthropic.com", "anthropic", "oauth"]);
        assert!(f.section().unwrap_err().to_string().contains("claude setup-token"));
        f.values[6] = "sk-ant-oat01-x".into();
        let (_, s) = f.section().unwrap();
        assert_eq!(account(&s), "provider:codex:https://api.anthropic.com");

        // A URL that no preset has stays; a preset's own URL is replaced with the preset's.
        f.values[1] = "mistral".into();
        f.provider_changed();
        f.values[2] = "https://mistral.example.edu/v1".into();
        f.values[6] = "k".into();
        let (t, _) = f.section().unwrap();
        assert_eq!(t["base_url"].as_str(), Some("https://mistral.example.edu/v1"));
        assert_eq!(t["protocol"].as_str(), Some("mistral"));
        let saved = Library::parse(Some(&format!("[providers.mistral]\n{t}")), None, [], None).unwrap();
        let edited = SetupForm::new(Kind::Provider, "mistral", &saved);
        assert_eq!(edited.values, ["mistral", "", "https://mistral.example.edu/v1", "mistral", "api-key", "", ""], "editing fills the form");

        let mut g = SetupForm::new(Kind::Provider, "local", &lib);
        g.values[0] = "local".into();
        g.values[1] = "openai-compatible".into();
        g.provider_changed();
        assert_eq!(g.values[2..5], ["", "openai", "api-key"]);
        g.values[2] = "https://localhost:8080/v1".into();
        g.values[4] = "none".into();
        let (t, _) = g.section().unwrap();
        assert_eq!(t["auth"].as_str(), Some("none"));
        g.values[2] = "http://localhost:8080/v1".into();
        assert!(g.section().unwrap_err().to_string().contains("https://"));
        g.values[2] = "https://localhost:8080/v1".into();
        g.values[3] = "anthropic".into();
        assert!(g.section().unwrap_err().to_string().contains("anthropic does not take auth"));
        g.values[3] = "gemini".into();
        assert!(g.section().is_err());
        g.values[3] = "openai".into();
        g.values[4] = "api-key".into();
        assert!(g.section().unwrap_err().to_string().contains("needs an API key"));

        let hints = |f: &SetupForm| f.fields().iter().map(|(l, h)| (l.clone(), *h)).collect::<Vec<_>>();
        let mut hinted = SetupForm::new(Kind::Provider, "cx", &lib);
        hinted.values[1] = "codex".into();
        hinted.provider_changed();
        let (_, token) = hints(&hinted).into_iter().find(|(l, _)| l == "Key / token").unwrap();
        assert!(token.contains("press Enter") && !token.contains("claude"), "{token}");
    }

    #[test]
    fn builds_profiles_on_saved_providers() {
        let cfg = "[providers.claude]\nbase_url = \"https://api.anthropic.com\"\nprotocol = \"anthropic\"\nauth = \"oauth\"\n[providers.codex]\nbase_url = \"https://api.openai.com/v1\"\nprotocol = \"openai-responses\"\nauth = \"oauth\"\n";
        let lib = Library::parse(Some(&format!("{cfg}[llm.cx]\nprovider = \"codex\"\nmodel = \"gpt-5.1\"\n")), None, [], None).unwrap();
        let f = SetupForm::new(Kind::Llm, "cx", &lib);
        assert_eq!(f.values, ["cx", "codex", "gpt-5.1"], "an existing profile fills its form");
        let mut g = SetupForm::new(Kind::Llm, "new", &lib);
        assert_eq!(g.values, ["new", "", ""]);
        g.values[1] = "codex".into();
        g.values[2] = "gpt-5.1".into();
        let (t, s) = g.section().unwrap();
        assert_eq!(t.to_string(), "provider = \"codex\"\nmodel = \"gpt-5.1\"\n");
        assert!(account(&s).is_empty());
        let saved = Library::parse(Some(&format!("{cfg}[llm.new]\n{t}")), None, [], None).unwrap();
        let (name, b) = saved.llm_profile(Some("new")).unwrap();
        assert_eq!((name.as_str(), b.model.as_str(), b.provider.as_str(), b.url()), ("new", "gpt-5.1", "codex", "https://api.openai.com/v1"));
        g.values[2].clear();
        assert!(g.section().unwrap_err().to_string().contains("model"));
        g.values[1].clear();
        g.values[2] = "m".into();
        assert!(g.section().unwrap_err().to_string().contains(":setup provider"));

        // A profile that names a provider the library lacks is refused when it is tested.
        let orphans = Library::parse(Some("[llm.ghost]\nprovider = \"nowhere\"\nmodel = \"m\"\n"), None, [], None).unwrap();
        let mut ghost = SetupForm::new(Kind::Llm, "ghost", &orphans);
        ghost.values.clone_from_slice(&["ghost".into(), "nowhere".into(), "m".into()]);
        let (_, orphan) = ghost.section().unwrap();
        assert!(test(&orphans.providers, &orphan, None).unwrap_err().to_string().contains("no provider named 'nowhere'"));
    }

    #[test]
    fn a_profile_tests_with_its_providers_credential() {
        let cfg = "[providers.cx]\nbase_url = \"https://api.openai.com/v1\"\nprotocol = \"openai-responses\"\nauth = \"oauth\"\n[providers.local]\nbase_url = \"https://localhost:8080/v1\"\nprotocol = \"openai\"\nauth = \"none\"\n[llm.a]\nprovider = \"cx\"\nmodel = \"gpt-5.1\"\n[llm.b]\nprovider = \"cx\"\nmodel = \"gpt-5.1-mini\"\n[llm.c]\nprovider = \"local\"\nmodel = \"qwen\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let lookup = |account: &str| Ok::<_, anyhow::Error>((account == "provider:cx:https://api.openai.com/v1").then(|| "cred".to_string()));
        let (a, key) = profile_credential(&lib.providers, "a", &lib.llm["a"], lookup).unwrap();
        assert_eq!((a.url(), a.model.as_str(), key.as_deref()), ("https://api.openai.com/v1", "gpt-5.1", Some("cred")));
        let (b, key) = profile_credential(&lib.providers, "b", &lib.llm["b"], lookup).unwrap();
        assert_eq!((a.provider.as_str(), b.provider.as_str(), key.as_deref()), ("cx", "cx", Some("cred")));
        let (c, key) = profile_credential(&lib.providers, "c", &lib.llm["c"], lookup).unwrap();
        assert!((c.url(), key.as_deref()) == ("https://localhost:8080/v1", None));

        let err = profile_credential(&lib.providers, "a", &lib.llm["a"], |_| Ok(None)).unwrap_err().to_string();
        assert!(err.contains("provider 'cx'") && err.contains(":setup provider"), "{err}");
        let err = profile_credential(&lib.providers, "a", &lib.llm["a"], |_| Err(anyhow::anyhow!("secret store unavailable"))).unwrap_err().to_string();
        assert!(err.contains("secret store unavailable"), "{err}");

        let orphan = Library::parse(Some("[llm.x]\nprovider = \"nowhere\"\nmodel = \"m\"\n"), None, [], None).unwrap();
        assert!(profile_credential(&orphan.providers, "x", &orphan.llm["x"], |_| Ok(None)).unwrap_err().to_string().contains("no provider named 'nowhere'"));
    }

    #[test]
    fn a_profile_test_sends_the_credential_it_is_given() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let provider = Provider { base_url: format!("http://{}/v1", server.local_addr().unwrap()), protocol: Protocol::OpenaiResponses, auth: Auth::Oauth, alias: None };
        let providers = BTreeMap::from([("cx".to_string(), provider)]);
        let setup = Setup::Llm("a".into(), Profile { provider: "cx".into(), model: "gpt-5.1".into() });
        // Past the credential check, the plain-http address is what stops the request.
        let err = test(&providers, &setup, Some("k")).unwrap_err().to_string();
        assert!(err.contains("only go over https://") && !err.contains("credential"), "{err}");
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn overview_lists_providers_before_profiles() {
        let cfg = "[providers.claude]\nbase_url = \"https://api.anthropic.com\"\nprotocol = \"anthropic\"\nauth = \"oauth\"\n[llm.a]\nprovider = \"claude\"\nmodel = \"m\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let rows = overview(&lib);
        let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
        let provider = names.iter().position(|r| *r == "LLM providers").unwrap();
        let profile = names.iter().position(|r| *r == "LLM profiles").unwrap();
        assert!(provider < profile, "{names:?}");
        assert_eq!((rows[provider][1].as_str(), rows[provider][2].as_str()), ("", "setup provider"));
        assert_eq!(rows[profile][1], "a");
    }

    #[test]
    fn entries_mark_the_default() {
        let cfg = "default_llm = \"b\"\n[providers.p]\nbase_url = \"https://x.example\"\nprotocol = \"openai\"\nauth = \"none\"\n[llm.a]\nprovider = \"p\"\nmodel = \"m\"\n[llm.b]\nprovider = \"p\"\nmodel = \"n\"\n[fulltext]\nlocation = \"dav\"\n[locations.dav]\nkind = \"webdav\"\nurl = \"https://dav.example/\"\nuser = \"u\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let llm = entries(Kind::Llm, &lib);
        assert_eq!((llm[0].1.as_str(), llm[1].1.as_str()), ("p · m", "p · n · default"));
        assert!(entries(Kind::Storage, &lib)[0].1.ends_with(" · default"));
        assert!(entries(Kind::Openalex, &lib).is_empty());
    }

    #[test]
    fn an_existing_provider_fills_its_form_and_keeps_its_key() {
        let cfg = "[providers.claude]\nbase_url = \"https://api.anthropic.com\"\nprotocol = \"anthropic\"\nauth = \"api-key\"\nalias = \"work\"\n";
        let lib = Library::parse(Some(cfg), None, [], None).unwrap();
        let mut f = SetupForm::new(Kind::Provider, "claude", &lib);
        assert_eq!(f.values, ["claude", "claude", "https://api.anthropic.com", "anthropic", "api-key", "work", ""]);
        assert_eq!(f.origin, Some(("claude".into(), "https://api.anthropic.com".into())));
        let (t, setup) = f.section().unwrap();
        assert_eq!(t["base_url"].as_str(), Some("https://api.anthropic.com"));
        assert_eq!(account(&setup), "provider:claude:https://api.anthropic.com");
        f.values[2] = "https://proxy.example".into();
        let (_, setup) = f.section().unwrap();
        assert_eq!(account(&setup), "provider:claude:https://proxy.example");
        let mut fresh = SetupForm::new(Kind::Provider, "other", &lib);
        fresh.values[2..5].clone_from_slice(&["https://x.example".into(), "openai".into(), "api-key".into()]);
        assert!(fresh.origin.is_none() && fresh.section().unwrap_err().to_string().contains("needs an API key"));
    }

    #[test]
    fn builds_and_checks_sections() {
        let lib = Library::parse(Some("[providers.local]\nbase_url = \"https://localhost:8080/v1\"\nprotocol = \"openai\"\nauth = \"none\"\n"), None, [], None).unwrap();
        let mut f = SetupForm::new(Kind::Storage, "dav", &lib);
        assert_eq!(f.values, ["dav", ""]);
        f.values[1] = "webdav".into();
        f.storage_kind_changed();
        assert_eq!((f.values.len(), f.fields().len(), f.masked(5)), (6, 6, true));
        f.values[2..].clone_from_slice(&["https://dav.example.com/pdfs".into(), "me".into(), "work".into(), "pw".into()]);
        let (t, s) = f.section().unwrap();
        assert!(!t.contains_key("password"));
        assert_eq!((t["kind"].as_str(), t["alias"].as_str()), (Some("webdav"), Some("work")));
        f.values[4] = "my-work".into();
        assert!(f.section().unwrap_err().to_string().contains("letters and digits"));
        f.values[4] = "work".into();
        f.values[2] = "http://nas.local/pdfs".into();
        assert!(f.section().unwrap_err().to_string().contains("only go over https://"));
        assert_eq!(account(&s), "webdav:me@https://dav.example.com/pdfs");

        let mut cloud = SetupForm::new(Kind::Storage, "box", &lib);
        assert!(cloud.section().unwrap_err().to_string().contains("local, webdav or dropbox"));
        cloud.values[1] = "dropbox".into();
        cloud.storage_kind_changed();
        cloud.values[2] = "/yeda/".into();
        let (t, s) = cloud.section().unwrap();
        assert_eq!((t["kind"].as_str(), t["folder"].as_str(), cloud.values.len()), (Some("dropbox"), Some("yeda"), 4));
        assert_eq!(account(&s), "dropbox:box");
        assert!(cloud.masked(3) && !cloud.masked(2));

        // Switching kind keeps the name and the where, and drops the rest.
        cloud.values[3] = "a code".into();
        cloud.values[1] = "local".into();
        cloud.storage_kind_changed();
        assert_eq!(cloud.values, ["box", "local", "/yeda/"]);
        assert!(!cloud.masked(2) && cloud.secret().is_none());
        let (t, s) = cloud.section().unwrap();
        assert!(t.is_empty());
        assert!(matches!(&s, Setup::Local(name, path) if name == "box" && path == "/yeda/"));
        assert_eq!(account(&s), "");
        cloud.values[2].clear();
        assert!(cloud.section().unwrap_err().to_string().contains("path"));

        let saved = Library::parse(Some("[locations.box]\nkind = \"dropbox\"\nfolder = \"pdfs\"\n\n[locations.here]\nkind = \"local\"\npath = \"/p\"\n"), None, [], None).unwrap();
        assert_eq!(SetupForm::new(Kind::Storage, "box", &saved).values, ["box", "dropbox", "pdfs", ""]);
        assert_eq!(SetupForm::new(Kind::Storage, "here", &saved).values, ["here", "local", "/p"]);

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
