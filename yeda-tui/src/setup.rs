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
            Kind::Openalex => 0,
            _ if name.is_empty() => 0,
            _ => 1,
        };
        let old = match kind {
            Kind::Search => Some(name.to_string()),
            Kind::Reference => Some(name.to_string()).filter(|n| !n.is_empty()),
            _ => None,
        };
        SetupForm { kind, values, sel, testing: false, old }
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
    pub fn fields(&self) -> &'static [(&'static str, &'static str)] {
        match self.kind {
            Kind::Llm => &[
                ("Name", "used as :summarize <table> <name>"),
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
        }
    }

    pub fn name(&self) -> &str {
        if self.kind == Kind::Openalex { "openalex" } else { self.values[0].trim() }
    }

    fn has_secret(&self) -> bool {
        !matches!(self.kind, Kind::Search | Kind::Reference)
    }

    pub fn secret(&self) -> Option<String> {
        Some(self.values.last()?.clone()).filter(|s| !s.is_empty() && self.has_secret())
    }

    pub fn masked(&self, field: usize) -> bool {
        self.has_secret() && field == self.values.len() - 1
    }

    /// The `[llm.<name>]` or `[locations.<name>]` table, checked by reading it back as yeda does.
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

/// Tests the connection; blocks.
pub fn test(setup: &Setup, secret: Option<&str>) -> Result<()> {
    match setup {
        Setup::Llm(p) => yeda_net::llm::ping(p, secret),
        Setup::Webdav(d) => yeda_net::webdav::login(d, secret.unwrap_or_default()),
        Setup::Search(_) | Setup::Reference(_) => Ok(()),
        Setup::Openalex => yeda_net::openalex::Client::new(secret.map(String::from)).one_by_doi("10.1038/nature16961").map(drop),
    }
}

/// The secret-store account the secret is kept under.
pub fn account(setup: &Setup) -> String {
    match setup {
        Setup::Llm(p) => yeda_secrets::llm_account(p.url()),
        Setup::Webdav(d) => yeda_secrets::webdav_account(&d.user, &d.url),
        Setup::Search(_) | Setup::Reference(_) => String::new(),
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
