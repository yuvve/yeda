use anyhow::{Context, Result};
use std::time::Duration;

fn handle_url(doi: &str) -> String {
    format!("https://doi.org/api/handles/{}", crate::oauth::encode(doi))
}

/// Checks the handle record without following it to the publisher.
pub fn exists(doi: &str) -> Result<bool> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).max_redirects(0).build().into();
    match agent.get(handle_url(doi)).call() {
        Ok(resp) if resp.status() == 200 => Ok(true),
        Ok(resp) => Err(ureq::Error::StatusCode(resp.status().as_u16())).context("doi.org"),
        Err(ureq::Error::StatusCode(404)) => Ok(false),
        Err(e) => Err(e).context("doi.org"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_literal_handle_in_the_path() {
        assert_eq!(handle_url("10.1000/../a#b?c=%2F+é"), "https://doi.org/api/handles/10.1000%2F..%2Fa%23b%3Fc%3D%252F%2B%C3%A9");
    }

    #[test]
    #[ignore = "network"]
    fn fetches_live() {
        assert!(exists("10.1038/nature14539").unwrap());
        assert!(exists("10.48550/arXiv.2006.11239").unwrap());
        assert!(!exists("10.9999/definitely-not-real-xyz").unwrap());
    }
}
