use serde::Deserialize;
use serde_json::{Value, json};

use anyhow::Result;

/// The wire dialect a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Anthropic,
    /// OpenAI chat completions: OpenAI, Ollama, llama.cpp, LM Studio, vLLM, …
    Openai,
    Mistral,
    Openrouter,
    /// The OpenCode Go subscription, through its OpenAI-compatible endpoint.
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    /// OpenAI's Responses API, which Codex OAuth tokens are accepted by.
    #[serde(rename = "openai-responses")]
    OpenaiResponses,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Anthropic => "anthropic",
            Protocol::Openai => "openai",
            Protocol::Mistral => "mistral",
            Protocol::Openrouter => "openrouter",
            Protocol::OpencodeGo => "opencode-go",
            Protocol::OpenaiResponses => "openai-responses",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    ApiKey,
    Oauth,
    None,
}

impl Auth {
    pub fn name(self) -> &'static str {
        match self {
            Auth::ApiKey => "api-key",
            Auth::Oauth => "oauth",
            Auth::None => "none",
        }
    }
}

/// A `[providers.<name>]` entry: where to connect and how. Its secret is shared by every profile
/// bound to it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub base_url: String,
    pub protocol: Protocol,
    pub auth: Auth,
}

impl Provider {
    /// Where requests, and so the credential, go.
    pub fn url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    pub fn needs_key(&self) -> bool {
        self.auth != Auth::None
    }

    /// Checks what a request would otherwise fail on: the address, and a credential the protocol
    /// cannot use.
    pub fn check(&self) -> Result<()> {
        if !self.base_url.starts_with("https://") {
            anyhow::bail!("{}: the base URL starts with https://", self.base_url);
        }
        match (self.protocol, self.auth) {
            (Protocol::Openai, Auth::ApiKey | Auth::None) => Ok(()),
            (Protocol::Anthropic, Auth::ApiKey | Auth::Oauth) => Ok(()),
            (Protocol::OpenaiResponses, Auth::Oauth) => Ok(()),
            (Protocol::Mistral | Protocol::Openrouter | Protocol::OpencodeGo, Auth::ApiKey) => Ok(()),
            (protocol, auth) => anyhow::bail!("{} does not take auth = \"{}\"", protocol.name(), auth.name()),
        }
    }
}

/// A `[llm.<name>]` entry: which provider to ask, and which of its models.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub provider: String,
    pub model: String,
}

/// A profile together with the provider it names: what one request needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub settings: Provider,
}

impl Bound {
    pub fn new(profile: &str, p: &Profile, provider: &str, settings: &Provider) -> Bound {
        Bound { profile: profile.into(), provider: provider.into(), model: p.model.clone(), settings: settings.clone() }
    }

    /// Where requests, and so the credential, go.
    pub fn url(&self) -> &str {
        self.settings.url()
    }

    pub fn protocol(&self) -> Protocol {
        self.settings.protocol
    }

    pub fn auth(&self) -> Auth {
        self.settings.auth
    }

    pub fn needs_key(&self) -> bool {
        self.settings.needs_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(text: &str) -> Result<Provider> {
        toml::from_str(text).map_err(Into::into)
    }

    #[test]
    fn providers_check_their_address_and_auth_pair() {
        let codex = provider("base_url = \"https://api.openai.com/v1\"\nprotocol = \"openai-responses\"\nauth = \"oauth\"\n").unwrap();
        assert_eq!((codex.url(), codex.protocol, codex.auth, codex.needs_key()), ("https://api.openai.com/v1", Protocol::OpenaiResponses, Auth::Oauth, true));
        codex.check().unwrap();
        let local = provider("base_url = \"https://localhost:8080/v1/\"\nprotocol = \"openai\"\nauth = \"none\"\n").unwrap();
        assert_eq!(local.url(), "https://localhost:8080/v1");
        assert!(!local.needs_key());
        local.check().unwrap();
        assert!(provider("base_url = \"https://x.test/v1\"\nprotocol = \"anthropic\"\nauth = \"none\"\n").unwrap().check().unwrap_err().to_string().contains("anthropic does not take auth = \"none\""));
        assert!(provider("base_url = \"https://x.test/v1\"\nprotocol = \"opencode-go\"\nauth = \"oauth\"\n").unwrap().check().is_err());
        assert!(provider("base_url = \"https://x.test/v1\"\nprotocol = \"openai-responses\"\nauth = \"api-key\"\n").unwrap().check().is_err());
        assert!(provider("base_url = \"http://x.test/v1\"\nprotocol = \"openai\"\nauth = \"api-key\"\n").unwrap().check().unwrap_err().to_string().contains("https://"));
        assert!(provider("base_url = \"https://x.test\"\nprotocol = \"gemini\"\nauth = \"api-key\"\n").is_err());
    }

    #[test]
    fn profiles_hold_only_a_provider_and_a_model() {
        let p: Profile = toml::from_str("provider = \"codex\"\nmodel = \"gpt-5.1\"\n").unwrap();
        assert_eq!((p.provider.as_str(), p.model.as_str()), ("codex", "gpt-5.1"));
        assert!(toml::from_str::<Profile>("provider = \"codex\"\nmodel = \"m\"\nbase_url = \"https://api.openai.com/v1\"\n").is_err());
        assert!(toml::from_str::<Profile>("provider = \"codex\"\nmodel = \"m\"\nmax_tokens = 100\n").is_err());
    }

    #[test]
    fn codex_credentials_parse_json_and_bare_tokens() {
        let json = serde_json::json!({"tokens": {
            "access_token": "eyJhbGciOiJub25lIn0.eyJleHAiOjE5MDAwMDAwMDB9.x",
            "refresh_token": "r",
            "oauth_provenance": {"issuer": "i", "client_id": "app_x"},
            "account_id": "acc",
        }});
        let cred = CodexCredential::parse(&json.to_string()).unwrap();
        assert_eq!(cred.access_token, "eyJhbGciOiJub25lIn0.eyJleHAiOjE5MDAwMDAwMDB9.x");
        assert_eq!(cred.refresh_token.as_deref(), Some("r"));
        assert_eq!(cred.client_id.as_deref(), Some("app_x"));
        assert_eq!(cred.account_id.as_deref(), Some("acc"));
        assert_eq!(cred.expires_at, Some(1_900_000_000));
        let bare = CodexCredential::parse(" eyJ-a\n eyJ-b ").unwrap();
        assert_eq!(bare.access_token, "eyJ-aeyJ-b");
        assert!(bare.refresh_token.is_none());
        assert!(CodexCredential::parse("{\"tokens\":{}}").is_err());
    }

    #[test]
    fn codex_credentials_know_when_to_renew_and_keep_their_identity() {
        let cred = CodexCredential {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            client_id: Some("app_x".into()),
            account_id: Some("acc".into()),
            host_id: Some("urn:uuid:1".into()),
            id_token: Some("id.jwt".into()),
            expires_at: Some(1_000),
        };
        assert!(cred.needs_refresh(600, 900));
        assert!(!cred.needs_refresh(600, 100));
        let json = serde_json::json!({
            "client_id": "app_x",
            "ext_agent_host_id": "urn:uuid:1",
            "id_token": "id.jwt",
            "tokens": {"access_token": "a2", "refresh_token": "r2", "oauth_provenance": {"client_id": "app_x"}, "account_id": "acc"},
        });
        assert_eq!(cred.refreshed("a2", Some("r2")), json.to_string());
        // A refresh round-trip keeps the identity, so the next sign-in reauthorizes the same one.
        let again = CodexCredential::parse(&json.to_string()).unwrap();
        assert_eq!(again.host_id.as_deref(), Some("urn:uuid:1"));
        assert_eq!(again.id_token.as_deref(), Some("id.jwt"));
        assert_eq!(again.client_id.as_deref(), Some("app_x"));
        let bare = CodexCredential { refresh_token: None, client_id: None, host_id: None, id_token: None, ..cred };
        assert_eq!(bare.refreshed("a3", None), "\"a3\"");
    }
}

/// Codex CLI credentials, as stored in the secret store: the compact `auth.json` JSON, or a bare
/// access token.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
    pub account_id: Option<String>,
    /// The stable host id yeda signed in with, and the id token for its next sign-in.
    pub host_id: Option<String>,
    pub id_token: Option<String>,
    /// Seconds since the epoch when the access token stops working.
    pub expires_at: Option<i64>,
}

impl CodexCredential {
    /// `auth_id` distinguishes which token's expiry matters when the pasted JSON is an
    /// `auth.json`; `expires_at` reads the access token's own `exp` claim.
    pub fn parse(key: &str) -> Result<CodexCredential> {
        let key = key.trim();
        if key.starts_with('{') {
            let v: Value = serde_json::from_str(key).map_err(|e| anyhow::anyhow!("{e:#}"))?;
            let tokens = &v["tokens"];
            let access = tokens["access_token"].as_str();
            let Some(access) = access else {
                anyhow::bail!("the Codex credentials have no tokens.access_token; sign in again with `codex login`");
            };
            Ok(CodexCredential {
                access_token: access.into(),
                refresh_token: tokens["refresh_token"].as_str().filter(|s| !s.is_empty()).map(String::from),
                client_id: tokens["oauth_provenance"]["client_id"].as_str().filter(|s| !s.is_empty()).map(String::from),
                account_id: tokens["account_id"].as_str().filter(|s| !s.is_empty()).map(String::from),
                host_id: v["ext_agent_host_id"].as_str().filter(|s| !s.is_empty()).map(String::from),
                id_token: v["id_token"].as_str().filter(|s| !s.is_empty()).map(String::from),
                expires_at: jwt_expires_at(access),
            })
        } else {
            let access = key.split_whitespace().collect::<String>();
            if access.is_empty() {
                anyhow::bail!("no Codex OAuth token");
            }
            let expires_at = jwt_expires_at(&access);
            Ok(CodexCredential { access_token: access, refresh_token: None, client_id: None, account_id: None, host_id: None, id_token: None, expires_at })
        }
    }

    /// The credential to store after a refresh keeps its account and client identity.
    pub fn refreshed(&self, access_token: &str, refresh_token: Option<&str>) -> String {
        let json = if self.refresh_token.is_some() || self.client_id.is_some() {
            let tokens = json!({
                "access_token": access_token,
                "refresh_token": refresh_token.unwrap_or_default(),
                "oauth_provenance": self.client_id.as_ref().map(|c| json!({"client_id": c})),
                "account_id": self.account_id,
            });
            json!({
                "client_id": self.client_id,
                "ext_agent_host_id": self.host_id,
                "id_token": self.id_token,
                "tokens": tokens,
            })
        } else {
            Value::String(access_token.into())
        };
        serde_json::to_string(&json).expect("CodexCredential::refreshed is valid JSON")
    }

    /// True when the access token runs out within `margin` seconds and a refresh token exists.
    pub fn needs_refresh(&self, margin: i64, now: i64) -> bool {
        self.refresh_token.is_some() && self.expires_at.is_some_and(|e| e - now < margin)
    }

    pub fn expired_message(&self) -> &'static str {
        if self.refresh_token.is_some() {
            "the Codex access token has expired; run `codex login` again and re-run the command"
        } else {
            "the Codex access token has expired; open its provider in :setup provider and press Enter to sign in again"
        }
    }
}

/// `exp` from a JWT payload; `None` when the token is not a three-part JWT.
fn jwt_expires_at(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v["exp"].as_i64()
}
