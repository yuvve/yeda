use anyhow::{Context, Result, bail};
use base64::Engine;
use std::time::Duration;
use yeda_core::library::Dav;
use yeda_core::webdav;

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

/// Downloads attachment `key` and returns the file `file` from it.
pub fn download(dav: &Dav, password: &str, key: &str, file: &str) -> Result<Vec<u8>> {
    let url = dav.file_url(&webdav::zip_name(key));
    let mut resp = agent().get(&url).header("Authorization", authorization(dav, password)).call().with_context(|| url.clone())?;
    check(&url, resp.status())?;
    let zip = resp.body_mut().with_config().limit(u64::MAX).read_to_vec().with_context(|| url.clone())?;
    webdav::unpack(&zip, file).with_context(|| url)
}

/// Uploads `bytes` as attachment `key` in Zotero's layout; `mtime_ms` is the file's modification time.
pub fn upload(dav: &Dav, password: &str, key: &str, file: &str, bytes: &[u8], mtime_ms: i64) -> Result<()> {
    let (zip, prop) = webdav::pack(file, bytes, mtime_ms)?;
    // The .prop goes last: Zotero takes its presence to mean the upload is complete.
    for (name, body) in [(webdav::zip_name(key), zip), (webdav::prop_name(key), prop.into_bytes())] {
        let url = dav.file_url(&name);
        let resp = agent().put(&url).header("Authorization", authorization(dav, password)).send(&body[..]).with_context(|| url.clone())?;
        check(&url, resp.status())?;
    }
    Ok(())
}
