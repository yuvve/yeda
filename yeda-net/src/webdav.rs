use anyhow::{Context, Result, bail};
use base64::Engine;
use std::time::Duration;
use yeda_core::library::Dav;

/// The server rejected the user name and password; ask for the password again.
#[derive(Debug)]
pub struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("the WebDAV server rejected the user name or password")
    }
}

impl std::error::Error for Unauthorized {}

pub fn is_unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unauthorized>().is_some()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10 * 60)))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .build()
        .into()
}

fn authorization(dav: &Dav, password: &str) -> String {
    let pair = format!("{}:{password}", dav.user);
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
}

fn check(url: &str, status: ureq::http::StatusCode) -> Result<()> {
    match status.as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err(Unauthorized.into()),
        404 => bail!("{url}: not found"),
        s => bail!("{url}: HTTP {s}"),
    }
}

/// The server's version of a file: its ETag, else its Last-Modified date. A cached copy with the
/// same version needs no download.
fn version(resp: &ureq::http::Response<ureq::Body>) -> Option<String> {
    let header = |name| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    header("etag").or_else(|| header("last-modified"))
}

/// The file at `path` and its version.
pub fn download(dav: &Dav, password: &str, path: &str) -> Result<(Vec<u8>, Option<String>)> {
    let url = dav.file_url(path);
    let mut resp = agent().get(&url).header("Authorization", authorization(dav, password)).call().with_context(|| url.clone())?;
    check(&url, resp.status())?;
    let version = version(&resp);
    let bytes = resp.body_mut().with_config().limit(u64::MAX).read_to_vec().with_context(|| url)?;
    Ok((bytes, version))
}

/// The version of the file at `path`, without downloading it.
pub fn current_version(dav: &Dav, password: &str, path: &str) -> Result<Option<String>> {
    let url = dav.file_url(path);
    let resp = agent().head(&url).header("Authorization", authorization(dav, password)).call().with_context(|| url.clone())?;
    check(&url, resp.status())?;
    Ok(version(&resp))
}

/// Uploads `bytes` to `path`, creating its folder first.
pub fn upload(dav: &Dav, password: &str, path: &str, bytes: &[u8]) -> Result<()> {
    if let Some((dir, _)) = path.rsplit_once('/') {
        let url = dav.file_url(dir) + "/";
        let mkcol = ureq::http::Request::builder().method("MKCOL").uri(&url).header("Authorization", authorization(dav, password)).body(())?;
        let resp = agent().run(mkcol).with_context(|| url.clone())?;
        // 405: the folder exists already.
        if resp.status().as_u16() != 405 {
            check(&url, resp.status())?;
        }
    }
    let url = dav.file_url(path);
    let resp = agent().put(&url).header("Authorization", authorization(dav, password)).send(bytes).with_context(|| url.clone())?;
    check(&url, resp.status())
}

/// Logs in and lists the server's folder, to test the URL, user and password.
pub fn login(dav: &Dav, password: &str) -> Result<()> {
    let url = dav.file_url("");
    let req = ureq::http::Request::builder().method("PROPFIND").uri(&url).header("Authorization", authorization(dav, password)).header("Depth", "0").body(())?;
    let resp = agent().run(req).with_context(|| url.clone())?;
    check(&url, resp.status())
}
