//! Dropbox as PDF storage, through the HTTP API with the "App folder" permission: paths are
//! relative to the folder Dropbox made for the app.
//!
//! Sign-in has no redirect: Dropbox shows a code, which the user pastes back.

use crate::oauth::{encode, pkce, token_response};
use crate::remote::{access_token, agent, check_status, client_id, json_or_error, read_limited};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use yeda_core::library::Cloud;

/// The app key of yeda's Dropbox app. Not a secret: the browser sees it in every sign-in link.
const APP_KEY: &str = "gzhh7xgpiedqdb9";
const AUTHORIZE: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN: &str = "https://api.dropboxapi.com/oauth2/token";
const RPC: &str = "https://api.dropboxapi.com/2/";
const CONTENT: &str = "https://content.dropboxapi.com/2/";
/// The most `files/upload` takes in one request.
const UPLOAD_LIMIT: usize = 150 << 20;

static TOKENS: Mutex<BTreeMap<String, (String, std::time::Instant)>> = Mutex::new(BTreeMap::new());

/// A sign-in waiting for the code Dropbox shows.
pub struct SignIn {
    pub url: String,
    pub verifier: String,
}


pub fn start(cloud: &Cloud) -> Result<SignIn> {
    let client_id = client_id(cloud, APP_KEY)?;
    let (verifier, challenge) = pkce();
    let query = [
        ("client_id", client_id.as_str()),
        ("response_type", "code"),
        ("token_access_type", "offline"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]
    .map(|(k, v)| format!("{k}={}", encode(v)))
    .join("&");
    Ok(SignIn { url: format!("{AUTHORIZE}?{query}"), verifier })
}

/// Trades the pasted code for the secret to keep: the refresh token.
pub fn finish(cloud: &Cloud, verifier: &str, code: &str) -> Result<String> {
    let client_id = client_id(cloud, APP_KEY)?;
    let form = [("grant_type", "authorization_code"), ("code", code.trim()), ("code_verifier", verifier), ("client_id", client_id.as_str())]
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .join("&");
    let mut resp = agent().post(TOKEN).header("content-type", "application/x-www-form-urlencoded").send(form).with_context(|| TOKEN.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if status == 400 && text.contains("invalid_grant") {
        bail!("Dropbox refused the code: it is wrong or expired; press Enter for a new link");
    }
    let v = token_response(TOKEN, status, &text)?;
    Ok(v["refresh_token"].as_str().context("Dropbox returned no refresh token")?.to_string())
}

fn token(cloud: &Cloud, secret: &str) -> Result<String> {
    access_token(&TOKENS, TOKEN, &client_id(cloud, APP_KEY)?, secret.trim())
}

/// `path` inside the location's folder, as Dropbox writes it: `/folder/path`.
fn full_path(cloud: &Cloud, path: &str) -> String {
    let segments: Vec<&str> = cloud.folder_segments().into_iter().chain(path.split('/').filter(|s| !s.is_empty())).collect();
    format!("/{}", segments.join("/"))
}

/// The `Dropbox-API-Arg` header: JSON in which every non-ASCII character is an escape, since a
/// header cannot carry them.
fn api_arg(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn rpc(token: &str, endpoint: &str, body: Option<&Value>) -> Result<Value> {
    let url = format!("{RPC}{endpoint}");
    let request = agent().post(&url).header("Authorization", format!("Bearer {token}"));
    let mut resp = match body {
        Some(body) => request.header("Content-Type", "application/json").send(body.to_string()),
        None => request.send_empty(),
    }
    .with_context(|| url.clone())?;
    json_or_error(&url, &mut resp)
}

fn content(token: &str, endpoint: &str, arg: &Value, body: Option<&[u8]>) -> Result<ureq::http::Response<ureq::Body>> {
    let url = format!("{CONTENT}{endpoint}");
    let request = agent().post(&url).header("Authorization", format!("Bearer {token}")).header("Dropbox-API-Arg", api_arg(arg));
    let mut resp = match body {
        Some(bytes) => request.header("Content-Type", "application/octet-stream").send(bytes),
        None => request.send_empty(),
    }
    .with_context(|| url.clone())?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let missing = status == 409 && text.contains("not_found");
    check_status(&url, if missing { 404 } else { status }, &text)?;
    Ok(resp)
}

pub fn download(cloud: &Cloud, secret: &str, path: &str) -> Result<(Vec<u8>, Option<String>)> {
    let token = token(cloud, secret)?;
    let mut resp = content(&token, "files/download", &json!({"path": full_path(cloud, path)}), None)?;
    let version = resp
        .headers()
        .get("dropbox-api-result")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .and_then(|v| v["rev"].as_str().map(String::from));
    let bytes = read_limited(&mut resp, path)?;
    Ok((bytes, version))
}

pub fn current_version(cloud: &Cloud, secret: &str, path: &str) -> Result<Option<String>> {
    let token = token(cloud, secret)?;
    let url = format!("{RPC}files/get_metadata");
    let body = json!({"path": full_path(cloud, path)});
    let mut resp = agent()
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .with_context(|| url.clone())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    check_status(&url, if status == 409 && text.contains("not_found") { 404 } else { status }, &text)?;
    let v: Value = serde_json::from_str(&text).with_context(|| format!("{url}: response is not JSON"))?;
    Ok(v["rev"].as_str().map(String::from))
}

pub fn upload(cloud: &Cloud, secret: &str, path: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() > UPLOAD_LIMIT {
        bail!("{path}: {} MB, more than the {} MB Dropbox takes in one upload", bytes.len() >> 20, UPLOAD_LIMIT >> 20);
    }
    let token = token(cloud, secret)?;
    let arg = json!({"path": full_path(cloud, path), "mode": "overwrite", "mute": true, "autorename": false});
    content(&token, "files/upload", &arg, Some(bytes))?;
    Ok(())
}

/// Checks the sign-in, and makes the location's folder if it is not there yet.
pub fn login(cloud: &Cloud, secret: &str) -> Result<()> {
    let token = token(cloud, secret)?;
    rpc(&token, "users/get_current_account", None)?;
    if cloud.folder_segments().is_empty() {
        return Ok(());
    }
    let url = format!("{RPC}files/create_folder_v2");
    let body = json!({"path": full_path(cloud, ""), "autorename": false});
    let mut resp = agent()
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .with_context(|| url.clone())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    // 409 conflict: the folder exists already.
    if status == 409 && text.contains("conflict") {
        return Ok(());
    }
    check_status(&url, status, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud(folder: &str) -> Cloud {
        Cloud { folder: folder.into(), client_id: Some("key".into()) }
    }

    #[test]
    fn paths_sit_inside_the_folder() {
        assert_eq!(full_path(&cloud("/yeda/"), "smith2020/a.pdf"), "/yeda/smith2020/a.pdf");
        assert_eq!(full_path(&cloud(""), "smith2020/a.pdf"), "/smith2020/a.pdf");
        assert_eq!(full_path(&cloud("yeda"), ""), "/yeda");
        assert_eq!(full_path(&cloud(""), ""), "/");
    }

    #[test]
    fn the_api_argument_is_ascii() {
        let arg = api_arg(&json!({"path": "/yeda/Müller 😀.pdf"}));
        assert!(arg.is_ascii(), "{arg}");
        assert_eq!(arg, "{\"path\":\"/yeda/M\\u00fcller \\ud83d\\ude00.pdf\"}");
        let back: Value = serde_json::from_str(&arg).unwrap();
        assert_eq!(back["path"], "/yeda/Müller 😀.pdf");
    }

    #[test]
    fn the_link_asks_for_a_code_with_a_refresh_token() {
        let sign_in = start(&cloud("yeda")).unwrap();
        assert!(sign_in.url.starts_with("https://www.dropbox.com/oauth2/authorize?client_id=key&"), "{}", sign_in.url);
        assert!(sign_in.url.contains("token_access_type=offline") && sign_in.url.contains("code_challenge_method=S256"), "{}", sign_in.url);
        assert!(!sign_in.url.contains("redirect_uri"), "{}", sign_in.url);
        assert!((43..=128).contains(&sign_in.verifier.len()));
    }

    #[test]
    fn without_a_client_id_the_built_in_app_is_used() {
        let none = Cloud { folder: String::new(), client_id: None };
        let sign_in = start(&none).unwrap();
        assert!(sign_in.url.contains(&format!("client_id={APP_KEY}")), "{}", sign_in.url);
    }
}
