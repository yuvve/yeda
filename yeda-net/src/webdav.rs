use crate::remote::{Unauthorized, agent, read_limited};
use anyhow::{Context, Result, bail};
use base64::Engine;
use yeda_core::library::Dav;

/// The Basic auth header, only for an https server: it carries the password.
fn authorization(dav: &Dav, password: &str) -> Result<String> {
    yeda_core::require_https(&dav.url)?;
    let pair = format!("{}:{password}", dav.user);
    Ok(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)))
}

fn check(url: &str, status: ureq::http::StatusCode) -> Result<()> {
    match status.as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err(Unauthorized("the WebDAV server rejected the user name or password".into()).into()),
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
    let mut resp = agent().get(&url).header("Authorization", authorization(dav, password)?).call().with_context(|| url.clone())?;
    check(&url, resp.status())?;
    let version = version(&resp);
    let bytes = read_limited(&mut resp, &url)?;
    Ok((bytes, version))
}

/// The version of the file at `path`, without downloading it.
pub fn current_version(dav: &Dav, password: &str, path: &str) -> Result<Option<String>> {
    let url = dav.file_url(path);
    let resp = agent().head(&url).header("Authorization", authorization(dav, password)?).call().with_context(|| url.clone())?;
    check(&url, resp.status())?;
    Ok(version(&resp))
}

/// Uploads `bytes` to `path`, creating its folder first.
pub fn upload(dav: &Dav, password: &str, path: &str, bytes: &[u8]) -> Result<()> {
    if let Some((dir, _)) = path.rsplit_once('/') {
        let url = dav.file_url(dir) + "/";
        let mkcol = ureq::http::Request::builder().method("MKCOL").uri(&url).header("Authorization", authorization(dav, password)?).body(())?;
        let resp = agent().run(mkcol).with_context(|| url.clone())?;
        // 405: the folder exists already.
        if resp.status().as_u16() != 405 {
            check(&url, resp.status())?;
        }
    }
    let url = dav.file_url(path);
    let resp = agent().put(&url).header("Authorization", authorization(dav, password)?).send(bytes).with_context(|| url.clone())?;
    check(&url, resp.status())
}

/// Logs in and lists the server's folder, to test the URL, user and password; a folder that
/// doesn't exist yet is created.
pub fn login(dav: &Dav, password: &str) -> Result<()> {
    let url = dav.file_url("");
    let req = ureq::http::Request::builder().method("PROPFIND").uri(&url).header("Authorization", authorization(dav, password)?).header("Depth", "0").body(())?;
    let resp = agent().run(req).with_context(|| url.clone())?;
    if resp.status().as_u16() != 404 {
        return check(&url, resp.status());
    }
    let mkcol = ureq::http::Request::builder().method("MKCOL").uri(&url).header("Authorization", authorization(dav, password)?).body(())?;
    let resp = agent().run(mkcol).with_context(|| url.clone())?;
    // 409: the folder above it is missing too.
    if resp.status().as_u16() == 409 {
        bail!("{url}: not found, and the folder above it doesn't exist either");
    }
    check(&url, resp.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_sends_the_password_over_plain_http() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let dav = Dav { url: format!("http://{}/dav", server.local_addr().unwrap()), user: "me".into(), alias: None };
        let err = login(&dav, "pw").unwrap_err().to_string();
        assert!(err.contains("only go over https://"), "{err}");
        assert!(upload(&dav, "pw", "a/b.pdf", b"%PDF").is_err());
        assert!(download(&dav, "pw", "a/b.pdf").is_err());
        assert!(current_version(&dav, "pw", "a/b.pdf").is_err());
        // Refused before connecting: the server saw nothing.
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
}
