//! Signing in with ChatGPT for a `codex` profile: yeda generates the authorization link, the
//! browser returns to a temporary loopback listener, and yeda exchanges the code for tokens.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// The first registration names the entrypoint client; the callback returns yeda's issued one.
const REGISTRATION_CLIENT: &str = "dynamic_agent_client";
const AGENT_NAME: &str = "yeda";
const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";

/// One pending sign-in: the listener is bound, the user only has to open `url`.
pub struct SignIn {
    pub url: String,
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    /// The issued client id of the credentials being re-signed-in, if any.
    issued_client_id: Option<String>,
    /// The stable host id of the credentials being re-signed-in, if any.
    host_id: Option<String>,
    listener: TcpListener,
}

/// Starts a sign-in: binds the callback listener and builds the link to open. A reauthorization
/// passes the stored `client_id`, host ID and id token; a first sign-in passes none.
pub fn start(issued_client_id: Option<&str>, host_id: Option<&str>, id_token_hint: Option<&str>) -> Result<SignIn> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding the sign-in callback listener")?;
    let port = listener.local_addr()?.port();
    let id = || uuid::Uuid::new_v4().simple().to_string();
    let (verifier, nonce, state) = (id(), id(), id());
    // A hyphenated UUID: OpenAI rejects the compact form.
    let host_id = host_id.map(String::from).unwrap_or_else(|| format!("urn:uuid:{}", uuid::Uuid::new_v4()));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let client_id = issued_client_id.unwrap_or(REGISTRATION_CLIENT);
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", client_id.into()),
        ("ext_agent_host_id", host_id.clone()),
        ("response_type", "code".into()),
        ("redirect_uri", format!("http://127.0.0.1:{port}/auth/callback")),
        ("scope", SCOPES.into()),
        ("resource", RESOURCE.into()),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge_method", "S256".into()),
        ("code_challenge", challenge),
    ];
    if issued_client_id.is_none() {
        params.push(("agent_name_hint", AGENT_NAME.into()));
    }
    if let Some(hint) = id_token_hint {
        params.push(("id_token_hint", hint.into()));
    }
    let query = params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&");
    Ok(SignIn { url: format!("{AUTHORIZE}?{query}"), state, nonce, verifier, issued_client_id: issued_client_id.map(String::from), host_id: Some(host_id), listener })
}

impl SignIn {
    /// Waits for the browser's callback and trades its code for the tokens; the returned JSON is
    /// what the secret store keeps.
    pub fn finish(self, timeout: Duration) -> Result<String> {
        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        let (mut stream, _) = 'wait: {
            loop {
                if Instant::now() >= deadline {
                    bail!("no sign-in callback arrived in {} minutes; start the sign-in again", timeout.as_secs() / 60);
                }
                match self.listener.accept() {
                    Ok(accepted) => break 'wait accepted,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(200)),
                    Err(e) => return Err(e).context("waiting for the sign-in callback"),
                }
            }
        };
        let query = callback_query(&mut stream, &self.state)?;
        let code = query.get("code").map(String::as_str).context("the callback carried no code")?;
        let issued = query.get("client_id").map(String::as_str).or(self.issued_client_id.as_deref()).context("the sign-in returned no client id")?;
        let answer = "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\nconnection: close\r\n\r\n\
            <title>yeda</title><p>Signed in. You can close this tab and go back to yeda.</p>";
        let _ = stream.write_all(answer.as_bytes());
        let _ = stream.flush();
        exchange(code, &self.verifier, issued, port_of(&self.listener), &self.nonce, self.host_id.as_deref())
    }
}

/// The callback's query parameters, from the request line only: answering before the browser
/// closes the connection keeps it from hanging on a keep-alive.
fn callback_query(stream: &mut TcpStream, expected_state: &str) -> Result<std::collections::HashMap<String, String>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
        }
    }
    let line = request.lines().next().unwrap_or_default();
    let query = line.split_whitespace().nth(1).and_then(|path| path.split_once('?')).map(|(_, q)| q).unwrap_or_default();
    let mut params = std::collections::HashMap::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(k), percent_decode(v));
    }
    if let Some(error) = params.get("error") {
        bail!("the sign-in was refused: {error}");
    }
    if params.get("state").map(String::as_str) != Some(expected_state) {
        bail!("the sign-in callback does not belong to this attempt");
    }
    Ok(params)
}

/// Trades the callback's code for tokens, checking the returned id token's claims.
fn exchange(code: &str, verifier: &str, client_id: &str, port: u16, nonce: &str, host_id: Option<&str>) -> Result<String> {
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", &redirect_uri),
        ("resource", RESOURCE),
        ("client_id", client_id),
    ]
    .map(|(k, v)| format!("{k}={}", encode(v)))
    .join("&");
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).https_only(true).build().into();
    let mut resp = agent
        .post(TOKEN)
        .header("content-type", "application/x-www-form-urlencoded")
        .send(form)
        .with_context(|| TOKEN.to_string())?;
    let status = resp.status();
    let text = resp.body_mut().read_to_string()?;
    if !status.is_success() {
        bail!("{TOKEN}: HTTP {status}: {}", text.chars().take(300).collect::<String>());
    }
    let v: Value = serde_json::from_str(&text).with_context(|| format!("{TOKEN}: response is not JSON"))?;
    let access = v["access_token"].as_str().context("the token response has no access_token")?;
    let refresh = v["refresh_token"].as_str().unwrap_or_default();
    let id_token = v["id_token"].as_str().unwrap_or_default();
    let claims = jwt_claims(id_token);
    if claims["iss"].as_str() != Some(ISSUER) {
        bail!("the sign-in's id token names another issuer");
    }
    if claims["aud"].as_str() != Some(client_id) {
        bail!("the sign-in's id token belongs to another client");
    }
    if claims["nonce"].as_str() != Some(nonce) {
        bail!("the sign-in's id token belongs to another attempt");
    }
    // The signature is not checked: the token arrives straight from OpenAI over https and is only
    // used against OpenAI, so there is no third party to fool with a forged one.
    let account = claims["chatgpt_account_id"].as_str().filter(|s| !s.is_empty());
    let json = json!({
        "client_id": client_id,
        "ext_agent_host_id": host_id,
        "id_token": id_token,
        "tokens": {
            "access_token": access,
            "refresh_token": refresh,
            "oauth_provenance": {"issuer": ISSUER, "client_id": client_id},
            "account_id": account,
        },
    });
    serde_json::to_string(&json).context("serializing the signed-in credentials")
}

fn jwt_claims(jwt: &str) -> Value {
    jwt.split('.').nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn port_of(listener: &TcpListener) -> u16 {
    listener.local_addr().map(|a| a.port()).unwrap_or_default()
}

/// Percent-encodes everything but the unreserved characters, as the docs ask for every value.
pub fn encode(v: &str) -> String {
    let mut out = String::new();
    for byte in v.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*byte as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_url_with_the_callback_and_challenge() {
        let sign_in = start(None, None, None).unwrap();
        assert!(sign_in.url.starts_with("https://auth.openai.com/api/accounts/authorize?"), "{}", sign_in.url);
        assert!(sign_in.url.contains("client_id=dynamic_agent_client"), "{}", sign_in.url);
        assert!(sign_in.url.contains("agent_name_hint=yeda"), "{}", sign_in.url);
        assert!(sign_in.url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"), "{}", sign_in.url);
        assert!(sign_in.url.contains("%2Fauth%2Fcallback"), "{}", sign_in.url);
        assert!(sign_in.url.contains("code_challenge_method=S256"), "{}", sign_in.url);
        assert!(!sign_in.url.contains(' '), "{}", sign_in.url);
        assert_ne!(sign_in.verifier, sign_in.state);
        // The host id is a hyphenated UUID, which OpenAI accepts; the compact form is refused.
        let at = sign_in.url.find("ext_agent_host_id=urn%3Auuid%3A").unwrap() + "ext_agent_host_id=urn%3Auuid%3A".len();
        let uuid = &sign_in.url[at..at + 36];
        assert_eq!(uuid.len(), 36, "{uuid}");
        assert_eq!(uuid.chars().filter(|c| *c == '-').count(), 4, "{uuid}");

        let issued = start(Some("oaiapp_x"), Some("urn:uuid:1"), Some("id.jwt")).unwrap();
        assert!(issued.url.contains("client_id=oaiapp_x"), "{}", issued.url);
        assert!(issued.url.contains("ext_agent_host_id=urn%3Auuid%3A1"), "{}", issued.url);
        assert!(issued.url.contains("id_token_hint=id.jwt"), "{}", issued.url);
        assert!(!issued.url.contains("agent_name_hint"), "{}", issued.url);
    }

    #[test]
    fn percent_decodes_query_values() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn reads_the_callback_without_waiting_for_the_connection_to_close() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let query_of = |request: String| {
            let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            std::thread::spawn(move || {
                let mut client = client;
                client.write_all(request.as_bytes()).unwrap();
                // A keep-alive connection: the browser does not close, so reading to EOF hangs.
                std::thread::sleep(Duration::from_secs(2));
                let _ = client.read_to_string(&mut String::new());
            });
            let (mut stream, _) = listener.accept().unwrap();
            callback_query(&mut stream, "s")
        };
        let ok = "GET /auth/callback?code=x&state=s&client_id=oaiapp_x HTTP/1.1\r\nhost: h\r\nconnection: keep-alive\r\n\r\n".to_string();
        let params = query_of(ok).unwrap();
        assert_eq!(params.get("code").map(String::as_str), Some("x"));
        assert_eq!(params.get("client_id").map(String::as_str), Some("oaiapp_x"));

        // A state that does not belong to this attempt is refused before the exchange.
        let bad = "GET /auth/callback?code=x&state=other HTTP/1.1\r\n\r\n".to_string();
        let mismatch = query_of(bad).unwrap_err();
        assert!(mismatch.to_string().contains("does not belong"), "{mismatch}");
    }
}
