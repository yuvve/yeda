use anyhow::{Result, bail};
use serde::Deserialize;
use yeda_core::library::{Dav, Library};
use yeda_core::llm::{Profile, Provider};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Llm,
    Webdav,
}

/// The `:setup` form for one LLM profile or WebDAV location; the last field is the secret.
pub struct SetupForm {
    pub kind: Kind,
    pub values: Vec<String>,
    pub sel: usize,
    /// The connection is being tested; the form waits.
    pub testing: bool,
}

/// What the form saves, once its connection works.
#[derive(Clone, Debug)]
pub enum Setup {
    Llm(Profile),
    Webdav(Dav),
}

impl SetupForm {
    /// Filled in from the profile or location `name`, if there is one.
    pub fn new(kind: Kind, name: &str, lib: &Library) -> SetupForm {
        let values = match kind {
            Kind::Llm => match lib.llm.get(name) {
                Some(p) => {
                    let provider = if p.provider == Provider::Anthropic { "anthropic" } else { "openai" };
                    vec![name.into(), provider.into(), p.model.clone(), p.base_url.clone().unwrap_or_default(), String::new()]
                }
                None => vec![name.into(), "anthropic".into(), String::new(), String::new(), String::new()],
            },
            Kind::Webdav => match lib.remotes.get(name) {
                Some(d) => vec![name.into(), d.url.clone(), d.user.clone(), String::new()],
                None => vec![name.into(), String::new(), String::new(), String::new()],
            },
        };
        let sel = if name.is_empty() { 0 } else { 1 };
        SetupForm { kind, values, sel, testing: false }
    }

    /// Each field's label and hint.
    pub fn fields(&self) -> &'static [(&'static str, &'static str)] {
        match self.kind {
            Kind::Llm => &[
                ("Name", "used as :summarize <table> <name>"),
                ("Provider", "anthropic, or openai for any OpenAI-compatible server"),
                ("Model", "e.g. claude-opus-5-5"),
                ("Base URL", "for a server other than the provider's own"),
                ("API key", "kept in the secret store; empty for a local server"),
            ],
            Kind::Webdav => &[
                ("Name", "used as pdf.location and in :attach"),
                ("URL", "the folder PDFs go in, e.g. https://dav.example.com/pdfs/"),
                ("User", ""),
                ("Password", "kept in the secret store"),
            ],
        }
    }

    pub fn name(&self) -> &str {
        self.values[0].trim()
    }

    pub fn secret(&self) -> Option<String> {
        Some(self.values.last()?.clone()).filter(|s| !s.is_empty())
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
                t.insert("provider".into(), v[1].to_lowercase().into());
                t.insert("model".into(), v[2].into());
                if !v[3].is_empty() {
                    t.insert("base_url".into(), v[3].into());
                }
                if v[2].is_empty() {
                    bail!("give the model's name");
                }
                let p = Profile::deserialize(toml::Value::Table(t.clone())).map_err(|_| anyhow::anyhow!("the provider is anthropic or openai"))?;
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
        };
        Ok((t, setup))
    }
}

/// Tests the connection; blocks.
pub fn test(setup: &Setup, secret: Option<&str>) -> Result<()> {
    match setup {
        Setup::Llm(p) => yeda_net::llm::ping(p, secret),
        Setup::Webdav(d) => yeda_net::webdav::login(d, secret.unwrap_or_default()),
    }
}

/// The secret-store account the secret is kept under.
pub fn account(setup: &Setup) -> String {
    match setup {
        Setup::Llm(p) => yeda_secrets::llm_account(p.key_env()),
        Setup::Webdav(d) => yeda_secrets::webdav_account(&d.user, &d.url),
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
        let (t, _) = f.section().unwrap();
        assert_eq!(t.get("base_url").and_then(|v| v.as_str()), Some("http://localhost:8080/v1"));

        let mut f = SetupForm::new(Kind::Llm, "claude", &lib);
        f.values[2] = "claude-opus-5-5".into();
        assert!(f.section().unwrap_err().to_string().contains("API key"));
        f.values[1] = "gemini".into();
        assert!(f.section().is_err());

        let mut f = SetupForm::new(Kind::Webdav, "dav", &lib);
        f.values[1..].clone_from_slice(&["https://dav.example.com/pdfs".into(), "me".into(), "pw".into()]);
        let (t, s) = f.section().unwrap();
        assert!(!t.contains_key("password"));
        assert_eq!(account(&s), "webdav:me@https://dav.example.com/pdfs");
    }
}
