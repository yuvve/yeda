//! Downloading a PDF over plain HTTP.

use anyhow::Result;
use std::time::Duration;
use yeda_core::fulltext::is_pdf;

const USER_AGENT: &str = concat!("yeda/", env!("CARGO_PKG_VERSION"), " (+https://github.com/yuvve/yeda)");
const MAX_BYTES: u64 = 200 << 20;

/// The open-access PDF at `url`. A login, challenge or landing page is an error: paywalled copies
/// come through the bundled browser, whose cookies only work from that browser.
pub fn download(url: &str) -> Result<Vec<u8>> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).user_agent(USER_AGENT).http_status_as_error(false).build().into();
    let mut resp = agent.get(url).call().map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = resp.body_mut().with_config().limit(MAX_BYTES).read_to_vec()?;
    if status >= 400 || !is_pdf(&bytes) {
        let kind = if kind.is_empty() { "no content type" } else { &kind };
        anyhow::bail!("{url}: not a PDF (HTTP {status}, {kind})");
    }
    Ok(bytes)
}
