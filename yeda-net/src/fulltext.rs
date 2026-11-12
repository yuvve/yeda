//! Downloading a PDF directly, without the bundled browser.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::time::Duration;
use yeda_core::fulltext::{direct_url, is_pdf, redirect};

const USER_AGENT: &str = concat!("yeda/", env!("CARGO_PKG_VERSION"), " (+https://github.com/yuvve/yeda)");

/// The largest PDF a direct download takes; papers are far smaller.
const MAX_BYTES: u64 = 100 << 20;

/// Redirects followed, each checked like the first URL.
const MAX_REDIRECTS: usize = 5;

/// The open-access PDF at `url`. A login, challenge or landing page is an error: paywalled copies
/// come through the bundled browser, whose cookies only work from that browser. Only `https://`
/// to a public host is fetched, after every redirect too (see `direct_url`).
pub fn download(url: &str) -> Result<Vec<u8>> {
    // ureq would follow redirects without the host check, so they are followed here.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .build()
        .into();
    let mut url = direct_url(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let mut resp = agent.get(&url).call().map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
        let status = resp.status().as_u16();
        if resp.status().is_redirection() {
            let location = resp.headers().get("location").and_then(|v| v.to_str().ok());
            let next = location.and_then(|l| redirect(&url, l)).with_context(|| format!("{url}: HTTP {status} without a usable Location"))?;
            url = direct_url(&next)?;
            continue;
        }
        let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let not_pdf = || {
            let kind = if kind.is_empty() { "no content type" } else { &kind };
            anyhow::anyhow!("{url}: not a PDF (HTTP {status}, {kind})")
        };
        if status >= 400 {
            return Err(not_pdf());
        }
        if let Some(len) = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok()).filter(|&len| len > MAX_BYTES) {
            bail!("{url}: {} MB, more than the {} MB limit", len >> 20, MAX_BYTES >> 20);
        }
        let mut body = resp.body_mut().with_config().limit(MAX_BYTES).reader();
        // The start tells a PDF from a page, so neither a page nor an endless stream is read whole.
        let mut bytes = Vec::new();
        (&mut body).take(1024).read_to_end(&mut bytes).with_context(|| url.clone())?;
        if !is_pdf(&bytes) {
            return Err(not_pdf());
        }
        body.read_to_end(&mut bytes).with_context(|| format!("{url} (limit {} MB)", MAX_BYTES >> 20))?;
        return Ok(bytes);
    }
    bail!("{url}: more than {MAX_REDIRECTS} redirects")
}

#[cfg(test)]
mod tests {
    use super::download;
    use std::net::TcpListener;

    #[test]
    fn refuses_local_addresses_without_connecting() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let port = server.local_addr().unwrap().port();
        for url in [format!("http://127.0.0.1:{port}/x.pdf"), format!("https://localhost:{port}/x.pdf"), format!("https://[::ffff:127.0.0.1]:{port}/")] {
            let e = download(&url).unwrap_err().to_string();
            assert!(e.contains("not a public host"), "{e}");
        }
        assert_eq!(server.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    #[ignore = "network"]
    fn downloads_live() {
        // http:// is upgraded; arXiv's abstract page isn't a PDF.
        assert!(yeda_core::fulltext::is_pdf(&download("http://arxiv.org/pdf/1706.03762").unwrap()));
        assert!(download("https://arxiv.org/abs/1706.03762").unwrap_err().to_string().contains("not a PDF"));
    }
}
