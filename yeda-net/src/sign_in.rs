//! Signing in for an `oauth` provider, as its sign-in preset describes: yeda builds the link, the
//! browser returns to a temporary loopback listener, and yeda trades the code for tokens.

use anyhow::{Context, Result, bail};
use base64::Engine;
use crate::oauth::{encode, token_response, wait_for_callback};
use serde_json::Value;
use std::net::TcpListener;
use std::time::Duration;
use yeda_core::llm::{OauthCredential, SignIn as Preset, Vars};

/// One pending sign-in: the listener is bound, the user only has to open `url`.
pub struct SignIn {
    pub url: String,
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    preset: &'static Preset,
    /// The client an earlier sign-in registered, if any.
    registered: Option<String>,
    listener: TcpListener,
}

/// Starts a sign-in: binds the callback listener and builds the link to open. `stored` is the
/// credential being signed in again, whose client and id token the link reuses.
pub fn start(preset: &'static Preset, stored: Option<&OauthCredential>) -> Result<SignIn> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding the sign-in callback listener")?;
    let port = listener.local_addr()?.port();
    let id = || uuid::Uuid::new_v4().simple().to_string();
    let (verifier, challenge) = crate::oauth::pkce();
    let (nonce, state) = (id(), id());
    let registered = stored.and_then(|c| c.client_id.clone());
    let mut params: Vec<(String, String)> = [
        ("client_id", registered.clone().unwrap_or_else(|| preset.client_id.clone())),
        ("response_type", "code".into()),
        ("redirect_uri", redirect_uri(port)),
        ("scope", preset.scope.clone()),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge_method", "S256".into()),
        ("code_challenge", challenge),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .into();
    if let Some(resource) = &preset.resource {
        params.push(("resource".into(), resource.clone()));
    }
    let vars = Vars { install: crate::llm::install_id(), session: "" };
    let extra = preset.authorize_params.iter().chain(preset.registration_params.iter().filter(|_| registered.is_none()));
    for (k, t) in extra {
        params.push((k.clone(), t.render(&vars).with_context(|| format!("sign-in parameter {k}"))?));
    }
    if let Some(hint) = stored.and_then(|c| c.id_token.clone()) {
        params.push(("id_token_hint".into(), hint));
    }
    let query = params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&");
    Ok(SignIn { url: format!("{}?{query}", preset.authorize_url), state, nonce, verifier, preset, registered, listener })
}

fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/auth/callback")
}

impl SignIn {
    /// Waits for the browser's callback and trades its code for the tokens; the returned JSON is
    /// what the secret store keeps.
    pub fn finish(self, timeout: Duration) -> Result<String> {
        let query = wait_for_callback(&self.listener, timeout, &self.state, "Signed in. You can close this tab and go back to yeda.")?;
        let code = query.get("code").map(String::as_str).context("the callback carried no code")?;
        // A server that registers a client names it in the callback.
        let client = query.get("client_id").cloned().or(self.registered.clone()).unwrap_or_else(|| self.preset.client_id.clone());
        let port = self.listener.local_addr().map(|a| a.port()).unwrap_or_default();
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.into()),
            ("code_verifier", self.verifier.clone()),
            ("redirect_uri", redirect_uri(port)),
            ("client_id", client.clone()),
        ];
        form.extend(self.preset.resource.clone().map(|r| ("resource", r)));
        let v = post_form(&self.preset.token_url, &form)?;
        let access = v["access_token"].as_str().context("the token response has no access_token")?;
        let id_token = v["id_token"].as_str().filter(|s| !s.is_empty());
        if let Some(issuer) = &self.preset.issuer {
            let claims = jwt_claims(id_token.unwrap_or_default());
            if claims["iss"].as_str() != Some(issuer) {
                bail!("the sign-in's id token names another issuer");
            }
            if !audience_has(&claims["aud"], &client) {
                bail!("the sign-in's id token belongs to another client");
            }
            if claims["nonce"].as_str() != Some(&self.nonce) {
                bail!("the sign-in's id token belongs to another attempt");
            }
            // The signature is not checked: the token arrives straight from the issuer over https
            // and goes back only to it, so there is no third party to fool with a forged one.
        }
        let scope = granted_scope(&v, self.preset.required_scope.as_deref())?;
        let cred = OauthCredential {
            access_token: access.into(),
            refresh_token: v["refresh_token"].as_str().filter(|s| !s.is_empty()).map(String::from),
            client_id: Some(client),
            id_token: id_token.map(String::from),
            scope: Some(scope).filter(|s| !s.is_empty()),
            expires_at: None,
        };
        Ok(cred.to_json())
    }
}

/// Trades the refresh token for a new access token, keeping the client; the token endpoint may
/// rotate the refresh token, which the result keeps.
pub fn refresh(preset: &Preset, cred: &OauthCredential) -> Result<OauthCredential> {
    let Some(refresh) = &cred.refresh_token else {
        bail!("the access token has expired; open its provider in :setup provider and press Enter to sign in again");
    };
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh.clone()),
        ("client_id", cred.client_id.clone().unwrap_or_else(|| preset.client_id.clone())),
    ];
    form.extend(preset.resource.clone().map(|r| ("resource", r)));
    let v = post_form(&preset.token_url, &form)?;
    let access = v["access_token"].as_str().context("the token refresh returned no access_token")?;
    Ok(cred.refreshed(access, v["refresh_token"].as_str().filter(|s| !s.is_empty())))
}

fn post_form(url: &str, form: &[(&str, String)]) -> Result<Value> {
    yeda_core::require_https(url)?;
    let body = form.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&");
    // The endpoint's error body carries the machine-readable reason it refused.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let mut resp = agent.post(url).header("content-type", "application/x-www-form-urlencoded").send(body).with_context(|| url.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    token_response(url, status.as_u16(), &text)
}

/// The scopes granted, refused when `required` is not among them.
fn granted_scope(response: &Value, required: Option<&str>) -> Result<String> {
    let scope = response["scope"].as_str().unwrap_or_default();
    if let Some(required) = required
        && !scope.split_whitespace().any(|s| s == required)
    {
        bail!("the sign-in did not grant {required}; approve it in the browser to use this provider");
    }
    Ok(scope.to_string())
}

fn jwt_claims(jwt: &str) -> Value {
    jwt.split('.').nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// `aud` is the client id, or an array that includes it.
fn audience_has(aud: &Value, client_id: &str) -> bool {
    match aud {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|v| v.as_str() == Some(client_id)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chatgpt() -> &'static Preset {
        yeda_core::llm::sign_in("chatgpt").unwrap()
    }

    #[test]
    fn a_grant_without_the_required_scope_is_refused() {
        let ok = json!({"scope": "openid chatgpt.tokens.use.direct offline_access"});
        assert_eq!(granted_scope(&ok, Some("chatgpt.tokens.use.direct")).unwrap(), "openid chatgpt.tokens.use.direct offline_access");
        let err = granted_scope(&json!({"scope": "openid email"}), Some("chatgpt.tokens.use.direct")).unwrap_err().to_string();
        assert!(err.contains("chatgpt.tokens.use.direct"), "{err}");
        assert_eq!(granted_scope(&Value::Null, None).unwrap(), "");
        assert!(audience_has(&json!("oaiapp_x"), "oaiapp_x"));
        assert!(audience_has(&json!(["oaiapp_x", "other"]), "oaiapp_x"));
        assert!(!audience_has(&json!("oaiapp_y"), "oaiapp_x"));
    }

    #[test]
    fn the_link_carries_the_presets_parameters() {
        crate::llm::set_install_id("11111111-2222-4333-8444-555555555555".into());
        let first = start(chatgpt(), None).unwrap();
        let n = first.verifier.chars().count();
        assert!((43..=128).contains(&n), "{n}");
        assert!(first.url.starts_with("https://auth.openai.com/api/accounts/authorize?"), "{}", first.url);
        for part in ["client_id=dynamic_agent_client", "agent_name_hint=yeda", "redirect_uri=http%3A%2F%2F127.0.0.1%3A", "code_challenge_method=S256", "resource=https%3A%2F%2Fapi.openai.com%2Fv1", "ext_agent_host_id=urn%3Auuid%3A11111111-2222-4333-8444-555555555555"] {
            assert!(first.url.contains(part), "{part}: {}", first.url);
        }
        assert!(!first.url.contains(' ') && first.verifier != first.state);

        let stored = OauthCredential { access_token: "a".into(), refresh_token: None, client_id: Some("oaiapp_x".into()), id_token: Some("id.jwt".into()), scope: None, expires_at: None };
        let again = start(chatgpt(), Some(&stored)).unwrap();
        assert!(again.url.contains("client_id=oaiapp_x") && again.url.contains("id_token_hint=id.jwt"), "{}", again.url);
        assert!(!again.url.contains("agent_name_hint"), "{}", again.url);
    }

    #[test]
    fn a_credential_without_a_refresh_token_asks_for_a_sign_in() {
        let bare = OauthCredential::parse("token").unwrap();
        assert!(refresh(chatgpt(), &bare).unwrap_err().to_string().contains("sign in again"));
    }
}
