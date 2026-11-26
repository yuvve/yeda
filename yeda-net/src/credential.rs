//! A request's secret, by the provider's auth. Protocols ask for it without knowing the kind.

use anyhow::{Context, Result, bail};
use yeda_core::llm::{Auth, OauthCredential, Provider, SignIn};

pub trait Credential {
    /// The secret a request carries, from what the secret store keeps; `None` sends none.
    fn secret(&self, stored: Option<&str>) -> Result<Option<String>>;
    /// Whether `stored` should be renewed before it is used, `now` in seconds since the epoch.
    fn refresh_due(&self, stored: &str, now: i64) -> bool;
    /// The renewed record to store in place of `stored`.
    fn refresh(&self, stored: &str) -> Result<String>;
}

pub fn of(p: &Provider) -> Result<Box<dyn Credential>> {
    Ok(match p.auth {
        Auth::ApiKey | Auth::None => Box::new(Key),
        Auth::Oauth => {
            let name = p.sign_in.as_deref().context("auth = \"oauth\" needs sign_in")?;
            Box::new(Oauth(yeda_core::llm::sign_in(name).with_context(|| format!("no sign-in preset \"{name}\""))?))
        }
    })
}

/// Keys hold no whitespace; a long token copied from a terminal can wrap onto several lines.
fn clean(key: Option<&str>) -> Option<String> {
    key.map(|k| k.split_whitespace().collect::<String>()).filter(|k| !k.is_empty())
}

struct Key;

impl Credential for Key {
    fn secret(&self, stored: Option<&str>) -> Result<Option<String>> {
        Ok(clean(stored))
    }

    fn refresh_due(&self, _: &str, _: i64) -> bool {
        false
    }

    fn refresh(&self, _: &str) -> Result<String> {
        bail!("an API key cannot be renewed; store a new one with :setup provider")
    }
}

struct Oauth(&'static SignIn);

impl Credential for Oauth {
    fn secret(&self, stored: Option<&str>) -> Result<Option<String>> {
        let Some(stored) = stored.filter(|s| !s.trim().is_empty()) else { return Ok(None) };
        Ok(Some(OauthCredential::parse(stored)?.access_token))
    }

    fn refresh_due(&self, stored: &str, now: i64) -> bool {
        OauthCredential::parse(stored).is_ok_and(|c| c.needs_refresh(5 * 60, now))
    }

    fn refresh(&self, stored: &str) -> Result<String> {
        Ok(crate::sign_in::refresh(self.0, &OauthCredential::parse(stored)?)?.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_lose_the_whitespace_a_terminal_wrapped_in() {
        assert_eq!(clean(Some(" sk-ant-api03-ab\ncd \n")).as_deref(), Some("sk-ant-api03-abcd"));
        assert_eq!(clean(Some(" \n")), None);
    }

    #[test]
    fn a_sign_in_sends_only_its_access_token() {
        let codex = of(yeda_core::llm::preset("codex").unwrap()).unwrap();
        let stored = serde_json::json!({"access_token": "eyJ-access", "refresh_token": "r", "client_id": "c"}).to_string();
        assert_eq!(codex.secret(Some(&stored)).unwrap().as_deref(), Some("eyJ-access"));
        assert_eq!(codex.secret(Some("pasted-token")).unwrap().as_deref(), Some("pasted-token"));
        assert!(codex.secret(Some("{}")).is_err());
        assert_eq!(codex.secret(None).unwrap(), None);
        let key = of(yeda_core::llm::preset("openrouter").unwrap()).unwrap();
        assert!(!key.refresh_due("k", 0) && key.refresh("k").is_err());
    }
}
