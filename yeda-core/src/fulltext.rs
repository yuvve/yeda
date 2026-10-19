//! Finding a reference's PDF: candidate URLs, the library proxy, and checking a download is the paper.

use crate::library::Ref;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `[fulltext]` in yeda.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Library proxies, tried in order, each with `{url}` for the address to open through it.
    pub proxies: Vec<String>,
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

/// `url` opened through `proxy`.
pub fn proxied(url: &str, proxy: &str) -> String {
    proxy.replace("{url}", url)
}

/// The proxy's host, such as `proxy.lib.ltu.se`; EZproxy serves proxied sites under it.
pub fn proxy_host(proxy: &str) -> Option<String> {
    let rest = proxy.split("://").nth(1)?;
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

/// IEEE Xplore's PDF for a `/document/<number>` or `stamp/stamp.jsp?arnumber=<number>` page,
/// directly or through the proxy.
/// What an HTML page says, for a log line: its title, else the start of its visible text.
pub fn page_gist(html: &str) -> String {
    let lower = html.to_lowercase();
    let title = lower.find("<title").and_then(|i| {
        let start = i + lower[i..].find('>')? + 1;
        Some(html[start..start + lower[start..].find("</title")?].to_string())
    });
    let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(t) = title.map(|t| squeeze(&t)).filter(|t| !t.is_empty()) {
        return t;
    }
    let mut text = String::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        text.push_str(&rest[..i]);
        text.push(' ');
        let tag = rest[i..].to_lowercase();
        // A script or style element's content isn't text on the page.
        let skip_to = ["<script", "<style"].iter().find(|t| tag.starts_with(**t)).and_then(|t| tag.find(&format!("</{}", &t[1..])));
        let after = skip_to.unwrap_or(0) + i;
        rest = match rest[after..].find('>') {
            Some(j) => &rest[after + j + 1..],
            None => "",
        };
    }
    text.push_str(rest);
    squeeze(&text).chars().take(200).collect()
}

/// What to do on an IEEE Xplore page, directly or through a proxy.
#[derive(Debug, PartialEq)]
pub enum Ieee {
    /// A document page: open its PDF page, as the PDF button does.
    Document { stamp: String },
    /// The PDF page, which shows the PDF in a frame: download it.
    Stamp { pdf: String },
}

pub fn ieee(page: &str) -> Option<Ieee> {
    let (scheme, rest) = page.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    if !(host == "ieeexplore.ieee.org" || host.starts_with("ieeexplore-ieee-org.")) {
        return None;
    }
    let (number, document) = match path.strip_prefix("document/") {
        Some(rest) => (rest.split(['/', '?', '#']).next()?, true),
        None => (path.strip_prefix("stamp/stamp.jsp?")?.split('&').find_map(|p| p.strip_prefix("arnumber="))?, false),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(match document {
        true => Ieee::Document { stamp: format!("{scheme}://{host}/stamp/stamp.jsp?tp=&arnumber={number}") },
        false => Ieee::Stamp { pdf: format!("{scheme}://{host}/stampPDF/getPDF.jsp?tp=&arnumber={number}&ref=") },
    })
}

/// ScienceDirect's PDF link for a `/science/article/pii/<PII>` page, directly or through the
/// proxy; the article page has no `citation_pdf_url`.
pub fn sciencedirect_pdf(page: &str) -> Option<String> {
    let (scheme, rest) = page.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    if !(host == "www.sciencedirect.com" || host.starts_with("www-sciencedirect-com.")) {
        return None;
    }
    let pii = path.split(['?', '#']).next()?.strip_prefix("science/article/pii/")?;
    if pii.is_empty() || !pii.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("{scheme}://{host}/science/article/pii/{pii}/pdfft"))
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

/// The most pages a PDF of `r` should have, or `None` for books and theses: a whole journal
/// issue passes `looks_like`, since it contains the article.
pub fn max_pages(r: &Ref) -> Option<u32> {
    if matches!(r.kind.as_str(), "book" | "phdthesis" | "mastersthesis" | "thesis" | "techreport" | "report") {
        return None;
    }
    let range = r.extra.get("pages").and_then(|p| p.as_str()).and_then(|p| p.split_once(['-', '–']));
    let count = range.and_then(|(a, b)| Some(b.trim().parse::<u32>().ok()?.checked_sub(a.trim().parse::<u32>().ok()?)? + 1));
    // ponytail: 100 pages for an article of unknown length; [fulltext] could set it if surveys hit it.
    Some(count.map_or(100, |n| (2 * n).max(n + 10)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_up_pages() {
        assert_eq!(page_gist("<html><head><TITLE> Access\n denied </TITLE></head><body>x</body></html>"), "Access denied");
        let challenge = "<html><head><title></title><script>var a = '<b>';</script><style>p{}</style></head><body><div>Max challenge attempts exceeded.</div> <p>Please refresh the page to try again!</p></body></html>";
        assert_eq!(page_gist(challenge), "Max challenge attempts exceeded. Please refresh the page to try again!");
    }

    #[test]
    fn limits_pages() {
        let r = |kind: &str, pages: Option<&str>| {
            let mut r = Ref { kind: kind.into(), ..Default::default() };
            if let Some(p) = pages {
                r.extra.insert("pages".into(), p.into());
            }
            r
        };
        assert_eq!(max_pages(&r("article", Some("0074-0081"))), Some(18));
        assert_eq!(max_pages(&r("article", Some("3897-3935"))), Some(78));
        assert_eq!(max_pages(&r("article", Some("9042"))), Some(100));
        assert_eq!(max_pages(&r("inproceedings", None)), Some(100));
        assert_eq!(max_pages(&r("book", Some("1-20"))), None);
    }

    #[test]
    fn finds_sciencedirect_pdfs() {
        let proxied = "https://www-sciencedirect-com.proxy.lib.ltu.se/science/article/pii/S092523121400808X";
        assert_eq!(sciencedirect_pdf(proxied), Some(format!("{proxied}/pdfft")));
        assert_eq!(sciencedirect_pdf("https://www.sciencedirect.com/science/article/pii/S0925?via%3Dihub").as_deref(), Some("https://www.sciencedirect.com/science/article/pii/S0925/pdfft"));
        assert_eq!(sciencedirect_pdf(&format!("{proxied}/pdfft")), None);
        assert_eq!(sciencedirect_pdf("https://www.sciencedirect.com/journal/x"), None);
    }

    #[test]
    fn finds_ieee_pages() {
        let proxied = "ieeexplore-ieee-org.proxy.lib.ltu.se";
        let stamp = |n: &str, host: &str| Some(Ieee::Document { stamp: format!("https://{host}/stamp/stamp.jsp?tp=&arnumber={n}") });
        assert_eq!(ieee("https://ieeexplore-ieee-org.proxy.lib.ltu.se/document/10015635/"), stamp("10015635", proxied));
        assert_eq!(ieee("https://ieeexplore.ieee.org/document/8993024"), stamp("8993024", "ieeexplore.ieee.org"));
        let pdf = Some(Ieee::Stamp { pdf: format!("https://{proxied}/stampPDF/getPDF.jsp?tp=&arnumber=10015635&ref=") });
        assert_eq!(ieee("https://ieeexplore-ieee-org.proxy.lib.ltu.se/stamp/stamp.jsp?tp=&arnumber=10015635"), pdf);
        assert_eq!(ieee("https://ieeexplore.ieee.org/search/x"), None);
        assert_eq!(ieee("https://link.springer.com/document/123"), None);
    }

    #[test]
    fn builds_urls_and_checks_pdfs() {
        let settings = Settings::default();
        let proxy = "https://login.proxy.lib.ltu.se/login?url={url}";
        assert_eq!(publisher_urls("10.1007/s10514-020-09902-z", &settings), ["https://link.springer.com/content/pdf/10.1007/s10514-020-09902-z.pdf"]);
        assert!(publisher_urls("10.1109/x", &settings).is_empty());
        assert_eq!(proxied("https://doi.org/10.1/x", proxy), "https://login.proxy.lib.ltu.se/login?url=https://doi.org/10.1/x");
        assert_eq!(proxy_host(proxy).as_deref(), Some("proxy.lib.ltu.se"));
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
