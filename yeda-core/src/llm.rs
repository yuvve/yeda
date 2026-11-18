use serde::Deserialize;
use serde_json::{Value, json};

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    /// OpenAI-compatible chat completions: OpenAI, Ollama, llama.cpp, LM Studio, vLLM, …
    Openai,
    Mistral,
    Openrouter,
    /// The OpenCode Go subscription, through its OpenAI-compatible endpoint.
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    /// OpenAI's Codex OAuth tokens, which use the ChatGPT subscription through the Responses API.
    Codex,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub provider: Provider,
    pub model: String,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    /// Anthropic `output_config.effort`.
    pub effort: Option<String>,
    /// Anthropic server-side refusal fallbacks; defaults on for models that support them.
    pub fallbacks: Option<bool>,
    /// Truncate the paper text sent to OpenAI-compatible servers (small local context windows).
    pub max_input_chars: Option<usize>,
    /// Ask the server to enforce the JSON schema; turn off for servers that reject `response_format`.
    pub json_schema: Option<bool>,
}

impl Profile {
    /// An OpenAI-compatible server needs a key only when it is OpenAI's own.
    pub fn needs_key(&self) -> bool {
        self.provider != Provider::Openai || self.base_url.is_none()
    }

    /// Where requests, and so the API key, go.
    pub fn url(&self) -> &str {
        let default = match self.provider {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::Openai => "https://api.openai.com/v1",
            Provider::Mistral => "https://api.mistral.ai/v1",
            Provider::Openrouter => "https://openrouter.ai/api/v1",
            Provider::OpencodeGo => "https://opencode.ai/inference/go/openai/v1",
            Provider::Codex => "https://api.openai.com/v1",
        };
        self.base_url.as_deref().unwrap_or(default).trim_end_matches('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            expires_at: Some(1_000),
        };
        assert!(cred.needs_refresh(600, 900));
        assert!(!cred.needs_refresh(600, 100));
        let json = serde_json::json!({"tokens": {"access_token": "a2", "refresh_token": "r2", "oauth_provenance": {"client_id": "app_x"}, "account_id": "acc"}});
        assert_eq!(cred.refreshed("a2", Some("r2")), json.to_string());
        let bare = CodexCredential { refresh_token: None, client_id: None, ..cred };
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
                expires_at: jwt_expires_at(access),
            })
        } else {
            let access = key.split_whitespace().collect::<String>();
            if access.is_empty() {
                anyhow::bail!("no Codex OAuth token");
            }
            let expires_at = jwt_expires_at(&access);
            Ok(CodexCredential { access_token: access, refresh_token: None, client_id: None, account_id: None, expires_at })
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
            json!({"tokens": tokens})
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
            "the Codex access token has expired; store the compacted `~/.codex/auth.json` (jq -c .) so yeda can refresh it"
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
