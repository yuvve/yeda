//! Google Drive as PDF storage, through the HTTP API with the `drive.file` scope: yeda only sees
//! the files and folders it made itself. A path is a chain of folders found by name, so the first
//! of several with one name wins.
//!
//! Sign-in returns to a loopback listener, like Codex's.

use crate::oauth::{encode, pkce, random_state, token_response, wait_for_callback};
use crate::remote::{Client, Credentials, access_token, agent, check_status, client_for, json_or_error, read_limited};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::Duration;
use yeda_core::library::Cloud;

/// The OAuth client of yeda's Google Cloud project; empty until one is registered (see
/// `:help storage`). Google treats the secret of a desktop client as public.
const CLIENT_ID: &str = "";
const CLIENT_SECRET: &str = "";
const AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const FILES: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const ABOUT: &str = "https://www.googleapis.com/drive/v3/about";
const FOLDER: &str = "application/vnd.google-apps.folder";

static TOKENS: Mutex<BTreeMap<String, (String, std::time::Instant)>> = Mutex::new(BTreeMap::new());

/// A sign-in whose listener is bound: the user only has to open `url`.
pub struct SignIn {
    pub url: String,
    state: String,
    verifier: String,
    redirect_uri: String,
    client: Client,
    listener: TcpListener,
}

fn client(cloud: &Cloud, own_secret: Option<&str>) -> Result<Client> {
    client_for("Google Drive", cloud, own_secret, (CLIENT_ID, CLIENT_SECRET))
}

/// `own_secret` is the client secret of the user's own OAuth client, if `cloud` names one.
pub fn start(cloud: &Cloud, own_secret: Option<&str>) -> Result<SignIn> {
    let client = client(cloud, own_secret)?;
    let listener = TcpListener::bind("127.0.0.1:0").context("binding the sign-in callback listener")?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let (verifier, challenge) = pkce();
    let state = random_state();
    let query = [
        ("client_id", client.id.as_str()),
        ("redirect_uri", &redirect_uri),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]
    .map(|(k, v)| format!("{k}={}", encode(v)))
    .join("&");
    Ok(SignIn { url: format!("{AUTHORIZE}?{query}"), state, verifier, redirect_uri, client, listener })
}

impl SignIn {
    /// Waits for the browser's callback and trades its code for the secret to keep.
    pub fn finish(self, timeout: Duration) -> Result<String> {
        let query = wait_for_callback(&self.listener, timeout, &self.state, "Signed in. You can close this tab and go back to yeda.")?;
        let code = query.get("code").map(String::as_str).context("the callback carried no code")?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", self.verifier.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", self.client.id.as_str()),
        ];
        if let Some(secret) = &self.client.secret {
            form.push(("client_secret", secret));
        }
        let body = form.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&");
        let mut resp = agent().post(TOKEN).header("content-type", "application/x-www-form-urlencoded").send(body).with_context(|| TOKEN.to_string())?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let v = token_response(TOKEN, status, &text)?;
        let scope = v["scope"].as_str().unwrap_or_default();
        if !scope.split_whitespace().any(|s| s == SCOPE) {
            bail!("the sign-in did not grant access to the files yeda makes; allow it and sign in again");
        }
        let refresh_token = v["refresh_token"].as_str().context("Google returned no refresh token")?.to_string();
        // Only an OAuth client of the user's own needs its secret kept: the built-in one is in the binary.
        let client_secret = self.client.secret.filter(|_| !CLIENT_ID.is_empty() && self.client.id != CLIENT_ID);
        Ok(Credentials { refresh_token, client_secret }.to_secret())
    }
}

fn token(cloud: &Cloud, secret: &str) -> Result<String> {
    let credentials = Credentials::parse(secret);
    let client = client(cloud, credentials.client_secret.as_deref())?;
    access_token(&TOKENS, TOKEN, &client, &credentials.refresh_token)
}

/// A string inside a Drive query: `\` and `'` are escaped.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

struct Found {
    id: String,
    version: Option<String>,
}

fn get(token: &str, url: &str, query: &[(&str, &str)]) -> Result<ureq::http::Response<ureq::Body>> {
    let mut request = agent().get(url).header("Authorization", format!("Bearer {token}"));
    for (k, v) in query {
        request = request.query(k, v);
    }
    request.call().with_context(|| url.to_string())
}

/// The child of `parent` called `name`: a folder or a file, as asked.
fn find(token: &str, parent: &str, name: &str, folder: bool) -> Result<Option<Found>> {
    let kind = if folder { format!("mimeType = '{FOLDER}'") } else { format!("mimeType != '{FOLDER}'") };
    let q = format!("name = {} and {} in parents and trashed = false and {kind}", quoted(name), quoted(parent));
    let mut resp = get(token, FILES, &[("q", &q), ("fields", "files(id,md5Checksum,headRevisionId)"), ("pageSize", "1"), ("spaces", "drive")])?;
    let v = json_or_error(FILES, &mut resp)?;
    Ok(v["files"].get(0).map(|f| Found { id: f["id"].as_str().unwrap_or_default().into(), version: f["md5Checksum"].as_str().or(f["headRevisionId"].as_str()).map(String::from) }))
}

fn make_folder(token: &str, parent: &str, name: &str) -> Result<String> {
    let body = json!({"name": name, "mimeType": FOLDER, "parents": [parent]});
    let mut resp = agent()
        .post(FILES)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .query("fields", "id")
        .send(body.to_string())
        .with_context(|| FILES.to_string())?;
    let v = json_or_error(FILES, &mut resp)?;
    v["id"].as_str().map(String::from).context("Drive returned no folder id")
}

/// The id of the folder at `segments` under My Drive; with `create`, folders that are missing are made.
fn folder(token: &str, segments: &[&str], create: bool) -> Result<Option<String>> {
    let mut id = "root".to_string();
    for name in segments {
        id = match find(token, &id, name, true)? {
            Some(found) => found.id,
            None if create => make_folder(token, &id, name)?,
            None => return Ok(None),
        };
    }
    Ok(Some(id))
}

/// The folders above the file at `path`, below the location's folder, and the file's name.
fn split<'a>(cloud: &'a Cloud, path: &'a str) -> Result<(Vec<&'a str>, &'a str)> {
    let mut segments: Vec<&str> = cloud.folder_segments().into_iter().chain(path.split('/').filter(|s| !s.is_empty())).collect();
    let name = segments.pop().context("an empty path")?;
    Ok((segments, name))
}

fn locate(token: &str, cloud: &Cloud, path: &str) -> Result<Found> {
    let (dirs, name) = split(cloud, path)?;
    let parent = folder(token, &dirs, false)?.with_context(|| format!("{path}: not found"))?;
    find(token, &parent, name, false)?.with_context(|| format!("{path}: not found"))
}

pub fn download(cloud: &Cloud, secret: &str, path: &str) -> Result<(Vec<u8>, Option<String>)> {
    let token = token(cloud, secret)?;
    let found = locate(&token, cloud, path)?;
    let url = format!("{FILES}/{}", found.id);
    let mut resp = get(&token, &url, &[("alt", "media")])?;
    check_status(path, resp.status().as_u16(), "")?;
    let bytes = read_limited(&mut resp, path)?;
    Ok((bytes, found.version))
}

pub fn current_version(cloud: &Cloud, secret: &str, path: &str) -> Result<Option<String>> {
    let token = token(cloud, secret)?;
    Ok(locate(&token, cloud, path)?.version)
}

/// A multipart upload body: the file's metadata, then its bytes.
fn multipart(boundary: &str, metadata: &Value, mime: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    body
}

pub fn upload(cloud: &Cloud, secret: &str, path: &str, bytes: &[u8]) -> Result<()> {
    let token = token(cloud, secret)?;
    let (dirs, name) = split(cloud, path)?;
    let parent = folder(&token, &dirs, true)?.context("no folder to upload to")?;
    let mime = if name.to_ascii_lowercase().ends_with(".pdf") { "application/pdf" } else { "application/octet-stream" };
    let mut resp = match find(&token, &parent, name, false)? {
        Some(existing) => {
            let url = format!("{UPLOAD}/{}", existing.id);
            agent().patch(&url).header("Authorization", format!("Bearer {token}")).header("Content-Type", mime).query("uploadType", "media").send(bytes).with_context(|| url)?
        }
        None => {
            let boundary = format!("yeda{}", random_state());
            let body = multipart(&boundary, &json!({"name": name, "parents": [parent]}), mime, bytes);
            agent()
                .post(UPLOAD)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", format!("multipart/related; boundary={boundary}"))
                .query("uploadType", "multipart")
                .send(&body)
                .with_context(|| UPLOAD.to_string())?
        }
    };
    json_or_error(UPLOAD, &mut resp)?;
    Ok(())
}

/// Checks the sign-in, and makes the location's folder if it is not there yet.
pub fn login(cloud: &Cloud, secret: &str) -> Result<()> {
    let token = token(cloud, secret)?;
    let mut resp = get(&token, ABOUT, &[("fields", "user")])?;
    json_or_error(ABOUT, &mut resp)?;
    folder(&token, &cloud.folder_segments(), true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_names_in_queries() {
        assert_eq!(quoted("a.pdf"), "'a.pdf'");
        assert_eq!(quoted(r"it's a\b"), r"'it\'s a\\b'");
    }

    #[test]
    fn a_path_splits_into_folders_and_a_file() {
        let cloud = Cloud { folder: "/yeda/pdfs/".into(), client_id: None };
        assert_eq!(split(&cloud, "smith2020/a.pdf").unwrap(), (vec!["yeda", "pdfs", "smith2020"], "a.pdf"));
        let root = Cloud { folder: String::new(), client_id: None };
        assert_eq!(split(&root, "a.pdf").unwrap(), (vec![], "a.pdf"));
        assert!(split(&root, "").is_err());
    }

    #[test]
    fn multipart_wraps_the_bytes() {
        let body = multipart("B", &json!({"name": "a.pdf"}), "application/pdf", b"%PDF");
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("--B\r\nContent-Type: application/json"), "{text}");
        assert!(text.contains("\r\n\r\n{\"name\":\"a.pdf\"}\r\n--B\r\nContent-Type: application/pdf\r\n\r\n%PDF\r\n--B--"), "{text}");
    }

    #[test]
    fn the_link_returns_to_a_loopback_port_with_the_drive_file_scope() {
        let cloud = Cloud { folder: String::new(), client_id: Some("id.apps.googleusercontent.com".into()) };
        let sign_in = start(&cloud, Some("secret")).unwrap();
        assert!(sign_in.url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?client_id=id.apps.googleusercontent.com&"), "{}", sign_in.url);
        assert!(sign_in.url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"), "{}", sign_in.url);
        assert!(sign_in.url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file"), "{}", sign_in.url);
        assert!(sign_in.url.contains("access_type=offline") && sign_in.url.contains("code_challenge_method=S256"), "{}", sign_in.url);
    }
}
