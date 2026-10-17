//! Finding a reference's PDF: candidate URLs, the library proxy, and checking a download is the paper.

use crate::library::Ref;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `[fulltext]` in yeda.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The library proxy, with `{url}` for the address to open through it.
    pub proxy: Option<String>,
    /// The `[locations]` entry fetched PDFs are attached to.
    pub location: Option<String>,
    /// Where the bundled browser saves downloads; clients have a default.
    pub downloads: Option<PathBuf>,
    /// DOI prefix → PDF URL template with `{doi}`, added to `PATTERNS`.
    pub patterns: BTreeMap<String, String>,
}

/// Publishers whose PDF sits at a URL built from the DOI.
// ponytail: a handful from the library; `[fulltext.patterns]` adds more without a release.
pub const PATTERNS: &[(&str, &str)] = &[
    ("10.1007", "https://link.springer.com/content/pdf/{doi}.pdf"),
    ("10.1145", "https://dl.acm.org/doi/pdf/{doi}"),
    ("10.2514", "https://arc.aiaa.org/doi/pdf/{doi}"),
    ("10.1177", "https://journals.sagepub.com/doi/pdf/{doi}"),
    ("10.1142", "https://www.worldscientific.com/doi/pdf/{doi}"),
    ("10.1137", "https://epubs.siam.org/doi/pdf/{doi}"),
    ("10.1002", "https://onlinelibrary.wiley.com/doi/pdfdirect/{doi}"),
];

/// The publisher PDF URLs to try for `doi`, configured ones first.
pub fn publisher_urls(doi: &str, settings: &Settings) -> Vec<String> {
    let prefix = doi.split('/').next().unwrap_or_default();
    let configured = settings.patterns.iter().map(|(p, t)| (p.as_str(), t.as_str()));
    configured.chain(PATTERNS.iter().copied()).filter(|(p, _)| *p == prefix).map(|(_, t)| t.replace("{doi}", doi)).collect()
}

/// `url` opened through the proxy, or as it is without one.
pub fn proxied(url: &str, settings: &Settings) -> String {
    settings.proxy.as_ref().map_or_else(|| url.to_string(), |p| p.replace("{url}", url))
}

/// The proxy's host, such as `proxy.lib.ltu.se`; EZproxy serves proxied sites under it.
pub fn proxy_host(settings: &Settings) -> Option<String> {
    let rest = settings.proxy.as_deref()?.split("://").nth(1)?;
    let host = rest.split(['/', '?', ':']).next()?;
    // `login.proxy.example.edu` and `proxy.example.edu` serve the same proxied names.
    Some(host.strip_prefix("login.").unwrap_or(host).to_string())
}

/// `url` on EZproxy's proxied host name: `https://link.springer.com/x` → `https://link-springer-com.<proxy_host>/x`.
pub fn proxied_host(url: &str, proxy_host: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    // EZproxy doubles existing hyphens so the dots it turns into hyphens stay unambiguous.
    let host = host.replace('-', "--").replace('.', "-");
    Some(format!("{scheme}://{host}.{proxy_host}{path}"))
}

/// IEEE Xplore's PDF for a `/document/<number>` page, directly or through the proxy, whose
/// bot check stops the page but not the PDF.
pub fn ieee_pdf(page: &str) -> Option<String> {
    let (scheme, rest) = page.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    if !(host == "ieeexplore.ieee.org" || host.starts_with("ieeexplore-ieee-org.")) {
        return None;
    }
    let number = path.strip_prefix("document/")?.split(['/', '?', '#']).next()?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{scheme}://{host}/stampPDF/getPDF.jsp?tp=&arnumber={number}&ref="))
}

pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.trim_ascii_start().starts_with(b"%PDF-")
}

/// A file name like Zotero's: `Author - 2020 - Title.pdf`.
pub fn file_name(r: &Ref) -> String {
    let author = r.authors.first().and_then(|a| a.split(',').next()).unwrap_or(&r.key);
    let year = r.year.map(|y| format!(" - {y}")).unwrap_or_default();
    let title: String = r.title.chars().take(80).collect();
    let name = format!("{author}{year} - {title}");
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || " -.,()'".contains(c) { c } else { ' ' }).collect();
    format!("{}.pdf", clean.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Whether a PDF's text is `r`: its DOI appears, or the start of its title does.
pub fn looks_like(text: &str, r: &Ref) -> bool {
    let text = letters(text);
    let doi = r.doi.as_deref().map(letters).filter(|d| d.len() > 6);
    // Line breaks, hyphenation and ligatures garble long titles, so only the first words are compared.
    let title: String = r.title.split_whitespace().take(6).map(letters).collect();
    doi.is_some_and(|d| text.contains(&d)) || (title.len() >= 12 && text.contains(&title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ieee_pdfs() {
        let pdf = |n: &str, host: &str| format!("https://{host}/stampPDF/getPDF.jsp?tp=&arnumber={n}&ref=");
        let proxied = "ieeexplore-ieee-org.proxy.lib.ltu.se";
        assert_eq!(ieee_pdf("https://ieeexplore-ieee-org.proxy.lib.ltu.se/document/10015635/"), Some(pdf("10015635", proxied)));
        assert_eq!(ieee_pdf("https://ieeexplore.ieee.org/document/8993024"), Some(pdf("8993024", "ieeexplore.ieee.org")));
        assert_eq!(ieee_pdf("https://ieeexplore.ieee.org/search/x"), None);
        assert_eq!(ieee_pdf("https://link.springer.com/document/123"), None);
    }

    #[test]
    fn builds_urls_and_checks_pdfs() {
        let settings = Settings { proxy: Some("https://login.proxy.lib.ltu.se/login?url={url}".into()), ..Default::default() };
        assert_eq!(publisher_urls("10.1007/s10514-020-09902-z", &settings), ["https://link.springer.com/content/pdf/10.1007/s10514-020-09902-z.pdf"]);
        assert!(publisher_urls("10.1109/x", &settings).is_empty());
        assert_eq!(proxied("https://doi.org/10.1/x", &settings), "https://login.proxy.lib.ltu.se/login?url=https://doi.org/10.1/x");
        assert_eq!(proxy_host(&settings).as_deref(), Some("proxy.lib.ltu.se"));
        assert_eq!(
            proxied_host("https://link.springer.com/content/pdf/10.1/x.pdf", "proxy.lib.ltu.se").as_deref(),
            Some("https://link-springer-com.proxy.lib.ltu.se/content/pdf/10.1/x.pdf")
        );

        let r = Ref { key: "k".into(), title: "Deep Learning for Robust Multi-Robot Exploration".into(), doi: Some("10.1109/TRO.2018.2853573".into()), ..Default::default() };
        assert!(looks_like("Deep learn-\ning for robust multi‐robot exploration\nAbstract", &r));
        assert!(looks_like("see doi:10.1109/tro.2018.2853573", &r));
        assert!(!looks_like("Something else entirely", &r));
        assert!(is_pdf(b"\n%PDF-1.7"));
        let r = Ref { authors: vec!["Smith, J.".into()], year: Some(2020), title: "A: B/C?".into(), ..r };
        assert_eq!(file_name(&r), "Smith - 2020 - A B C.pdf");
    }
}
