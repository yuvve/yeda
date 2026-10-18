//! Downloading a PDF over plain HTTP.

use anyhow::Result;
use std::time::Duration;
use yeda_core::fulltext::is_pdf;

const USER_AGENT: &str = concat!("yeda/", env!("CARGO_PKG_VERSION"), " (+https://github.com/yuvve/yeda)");
const MAX_BYTES: u64 = 200 << 20;

/// The PDF at `url`, sending `cookie` as the `Cookie` header. A login, challenge or landing page
/// is an error: it needs the bundled browser.
pub fn download(url: &str, cookie: Option<&str>) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).user_agent(USER_AGENT).build().into();
    let mut req = agent.get(url);
    if let Some(c) = cookie {
        req = req.header("Cookie", c);
    }
    let mut resp = req.call().map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = resp.body_mut().with_config().limit(MAX_BYTES).read_to_vec()?;
    if !is_pdf(&bytes) {
        return Err(NotPdf(format!("{url}: not a PDF ({})", if kind.is_empty() { "no content type" } else { &kind })).into());
    }
    Ok(bytes)
}

/// The server answered, but with something other than a PDF, such as a web page.
#[derive(Debug)]
pub struct NotPdf(String);

impl std::fmt::Display for NotPdf {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotPdf {}

pub fn is_not_pdf(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotPdf>().is_some()
}
