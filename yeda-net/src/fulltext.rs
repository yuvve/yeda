//! Downloading a PDF over plain HTTP.

use anyhow::Result;
use std::time::Duration;
use yeda_core::fulltext::is_pdf;

const USER_AGENT: &str = concat!("yeda/", env!("CARGO_PKG_VERSION"), " (+https://github.com/yuvve/yeda)");
const MAX_BYTES: u64 = 200 << 20;

/// The PDF at `url`, sending `cookie` as the `Cookie` header. A login, challenge or landing page
/// is an error: it needs the bundled browser.
pub fn download(url: &str, cookie: Option<&str>) -> Result<Vec<u8>> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).user_agent(USER_AGENT).http_status_as_error(false).build().into();
    let mut req = agent.get(url);
    if let Some(c) = cookie {
        req = req.header("Cookie", c);
    }
    let mut resp = req.call().map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = resp.body_mut().with_config().limit(MAX_BYTES).read_to_vec()?;
    if status >= 400 || !is_pdf(&bytes) {
        let kind = if kind.is_empty() { "no content type" } else { &kind };
        let page = String::from_utf8_lossy(&bytes[..bytes.len().min(PAGE_BYTES)]).into_owned();
        return Err(NotPdf { message: format!("{url}: not a PDF (HTTP {status}, {kind})"), status, page }.into());
    }
    Ok(bytes)
}

/// How much of a page that came instead of a PDF is kept, to show or summarise.
const PAGE_BYTES: usize = 512 << 10;

/// The server answered with something other than a PDF, such as a sign-in, challenge or error page.
#[derive(Debug)]
pub struct NotPdf {
    message: String,
    status: u16,
    pub page: String,
}

impl std::fmt::Display for NotPdf {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NotPdf {}

/// The server answered successfully, just not with a PDF; a refusal (4xx, 5xx) is not this.
pub fn is_not_pdf(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotPdf>().is_some_and(|n| n.status < 400)
}

/// The page that came instead of a PDF, refused or not.
pub fn page_instead(e: &anyhow::Error) -> Option<&str> {
    e.downcast_ref::<NotPdf>().map(|n| n.page.as_str())
}
