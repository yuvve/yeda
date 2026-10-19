use anyhow::{Result, bail};
use serde::Deserialize;
use yeda_core::browse::SortField;
use yeda_core::library::{Dav, Library, SavedSearch};
use yeda_core::llm::{Profile, Provider};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Llm,
    Webdav,
    /// Editing a saved search; nothing to test.
    Search,
    /// The optional OpenAlex API key, which only goes to the secret store.
    Openalex,
}

/// The `:setup` form for one LLM profile or WebDAV location, whose last field is the secret; or a saved search being edited.
pub struct SetupForm {
    pub kind: Kind,
    pub values: Vec<String>,
    pub sel: usize,
    /// The connection is being tested; the form waits.
    pub testing: bool,
    /// The saved search's name before editing.
    pub old: Option<String>,
    /// The edited profile's own `api_key_env`, kept when its provider has no preset one.
    key_env: Option<String>,
}

/// Providers the form knows: name, API, base URL, and the variable (and secret) its key goes under.
pub const PRESETS: [(&str, Provider, &str, Option<&str>); 5] = [
    ("anthropic", Provider::Anthropic, "https://api.anthropic.com", None),
    ("openai", Provider::Openai, "https://api.openai.com/v1", None),
    ("gemini", Provider::Openai, "https://generativelanguage.googleapis.com/v1beta/openai", Some("GEMINI_API_KEY")),
    ("mistral", Provider::Openai, "https://api.mistral.ai/v1", Some("MISTRAL_API_KEY")),
    ("openrouter", Provider::Openai, "https://openrouter.ai/api/v1", Some("OPENROUTER_API_KEY")),
];

fn preset(name: &str) -> Option<&'static (&'static str, Provider, &'static str, Option<&'static str>)> {
    PRESETS.iter().find(|p| p.0 == name.trim().to_lowercase())
}

/// What the form saves, once its connection works.
#[derive(Clone, Debug)]
pub enum Setup {
    Llm(Profile),
    Webdav(Dav),
    Search(SavedSearch),
    Openalex,
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
        };
        let sel = if name.is_empty() || kind == Kind::Openalex { 0 } else { 1 };
        let old = (kind == Kind::Search).then(|| name.to_string());
        let key_env = lib.llm.get(name).filter(|_| kind == Kind::Llm).and_then(|p| p.api_key_env.clone());
        SetupForm { kind, values, sel, testing: false, old, key_env }
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

    pub fn secret(&self) -> Option<String> {
        Some(self.values.last()?.clone()).filter(|s| !s.is_empty() && self.kind != Kind::Search)
    }

    pub fn masked(&self, field: usize) -> bool {
        self.kind != Kind::Search && field == self.values.len() - 1
    }

    /// The `[llm.<name>]` or `[locations.<name>]` table, checked by reading it back as yeda does.
    pub fn section(&self) -> Result<(toml::Table, Setup)> {
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
                if let Some(env) = known.and_then(|p| p.3).map(String::from).or(self.key_env.clone()) {
                    t.insert("api_key_env".into(), env.into());
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

/// Tests the connection; blocks.
pub fn test(setup: &Setup, secret: Option<&str>) -> Result<()> {
    match setup {
        Setup::Llm(p) => yeda_net::llm::ping(p, secret),
        Setup::Webdav(d) => yeda_net::webdav::login(d, secret.unwrap_or_default()),
        Setup::Search(_) => Ok(()),
        Setup::Openalex => yeda_net::openalex::Client::new(secret.map(String::from)).one_by_doi("10.1038/nature16961").map(drop),
    }
}

/// The secret-store account the secret is kept under.
pub fn account(setup: &Setup) -> String {
    match setup {
        Setup::Llm(p) => yeda_secrets::llm_account(p.key_env()),
        Setup::Webdav(d) => yeda_secrets::webdav_account(&d.user, &d.url),
        Setup::Search(_) => String::new(),
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
        assert_eq!((t["provider"].as_str(), t["api_key_env"].as_str()), (Some("openai"), Some("GEMINI_API_KEY")));
        assert_eq!(account(&s), "llm:GEMINI_API_KEY");
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
        let mut f = SetupForm::new(Kind::Search, "new", &lib);
        assert_eq!(f.values, ["new", "tag:x", "-author"]);
        f.values[2] = "year".into();
        let Ok((_, Setup::Search(s))) = f.section() else { panic!() };
        assert_eq!((s.sort, s.descending), (Some(SortField::Year), false));
        f.values[2] = "-pages".into();
        assert!(f.section().is_err());
    }
}
