//! PDF storage on a remote: WebDAV or Dropbox behind one set of operations. The `secret` every
//! function takes is what the secret store holds for the location: the WebDAV password, or the
//! Dropbox refresh token.

use crate::{MAX_BYTES, dropbox, webdav};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use yeda_core::library::{Cloud, Remote};

/// The remote rejected the credentials; ask for them again.
#[derive(Debug)]
pub struct Unauthorized(pub String);

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unauthorized {}

pub fn is_unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unauthorized>().is_some()
}

/// The file at `path` and its version.
pub fn download(remote: &Remote, secret: &str, path: &str) -> Result<(Vec<u8>, Option<String>)> {
    match remote {
        Remote::Webdav(d) => webdav::download(d, secret, path),
        Remote::Dropbox(c) => dropbox::download(c, secret, path),
    }
}

/// The version of the file at `path`, without downloading it. A cached copy with the same version
/// needs no download.
pub fn current_version(remote: &Remote, secret: &str, path: &str) -> Result<Option<String>> {
    match remote {
        Remote::Webdav(d) => webdav::current_version(d, secret, path),
        Remote::Dropbox(c) => dropbox::current_version(c, secret, path),
    }
}

/// Uploads `bytes` to `path`, creating its folder first.
pub fn upload(remote: &Remote, secret: &str, path: &str, bytes: &[u8]) -> Result<()> {
    match remote {
        Remote::Webdav(d) => webdav::upload(d, secret, path, bytes),
        Remote::Dropbox(c) => dropbox::upload(c, secret, path, bytes),
    }
}

/// Tests the credentials, and creates the location's folder if it is missing.
pub fn login(remote: &Remote, secret: &str) -> Result<()> {
    match remote {
        Remote::Webdav(d) => webdav::login(d, secret),
        Remote::Dropbox(c) => dropbox::login(c, secret),
    }
}

/// An agent that only speaks https: every request carries a token or a password.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .https_only(true)
        .build()
        .into()
}

/// A whole file in memory, so a server (or a wrong URL) that sends without end is cut off.
pub(crate) fn read_limited(resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> Result<Vec<u8>> {
    if let Some(len) = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok()).filter(|&len| len > MAX_BYTES) {
        bail!("{what}: {} MB, more than the {} MB limit", len >> 20, MAX_BYTES >> 20);
    }
    resp.body_mut().with_config().limit(MAX_BYTES).read_to_vec().with_context(|| format!("{what} (limit {} MB)", MAX_BYTES >> 20))
}

/// The JSON a response carries, or the error its status stands for.
pub(crate) fn json_or_error(what: &str, resp: &mut ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    check_status(what, status, &text)?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("{what}: response is not JSON"))
}

/// Turns a failing status into an error: 401 is [`Unauthorized`], the rest say what the server said.
pub(crate) fn check_status(what: &str, status: u16, body: &str) -> Result<()> {
    match status {
        200..=299 => Ok(()),
        401 => Err(Unauthorized(format!("{what}: the sign-in was rejected")).into()),
        404 => bail!("{what}: not found"),
        s => bail!("{what}: HTTP {s}: {}", body.chars().take(300).collect::<String>()),
    }
}

/// The OAuth client a Dropbox location signs in with: the one yeda was built with, or the
/// `client_id` the location names.
pub(crate) fn client_id(cloud: &Cloud, built_in: &str) -> Result<String> {
    if let Some(id) = cloud.client_id.as_deref().filter(|id| !id.is_empty()) {
        return Ok(id.into());
    }
    if built_in.is_empty() {
        bail!("this build of yeda has no Dropbox app; register your own and set client_id on the location (see :help storage)");
    }
    Ok(built_in.into())
}

type Cache = Mutex<BTreeMap<String, (String, Instant)>>;

/// An access token for the refresh token, from the endpoint at most once until it nears its end.
pub(crate) fn access_token(cache: &'static Cache, endpoint: &str, client_id: &str, refresh_token: &str) -> Result<String> {
    let key = format!("{client_id}:{refresh_token}");
    if let Some((token, expires)) = cache.lock().unwrap().get(&key)
        && Instant::now() + Duration::from_secs(60) < *expires
    {
        return Ok(token.clone());
    }
    let form = [("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", client_id)];
    let body = form.iter().map(|(k, v)| format!("{k}={}", crate::oauth::encode(v))).collect::<Vec<_>>().join("&");
    let mut resp = agent().post(endpoint).header("content-type", "application/x-www-form-urlencoded").send(body).with_context(|| endpoint.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    // The endpoint answers a revoked or expired refresh token with 400 invalid_grant.
    if status == 401 || (status == 400 && text.contains("invalid_grant")) {
        return Err(Unauthorized("the sign-in expired or was revoked; sign in again with :setup storage".into()).into());
    }
    let v = crate::oauth::token_response(endpoint, status, &text)?;
    let token = v["access_token"].as_str().context("the token response has no access_token")?.to_string();
    let lifetime = v["expires_in"].as_u64().unwrap_or(3600);
    cache.lock().unwrap().insert(key, (token.clone(), Instant::now() + Duration::from_secs(lifetime)));
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_built_in_client_points_at_the_guide() {
        let cloud = Cloud { folder: String::new(), client_id: None };
        let err = client_id(&cloud, "").unwrap_err().to_string();
        assert!(err.contains("client_id") && err.contains(":help storage"), "{err}");
        let own = Cloud { folder: String::new(), client_id: Some("mine".into()) };
        assert_eq!(client_id(&own, "").unwrap(), "mine");
        assert_eq!(client_id(&cloud, "built").unwrap(), "built");
        assert_eq!(client_id(&own, "built").unwrap(), "mine");
    }

    #[test]
    fn statuses_map_to_errors() {
        assert!(check_status("x", 200, "").is_ok());
        assert!(is_unauthorized(&check_status("x", 401, "").unwrap_err()));
        assert!(check_status("x", 404, "").unwrap_err().to_string().contains("not found"));
        assert!(check_status("x", 500, "boom").unwrap_err().to_string().contains("boom"));
    }
}
