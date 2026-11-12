//! Finding a reference's PDF: candidate URLs, the library proxy, and checking a download is the paper.

use crate::library::Ref;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
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

/// What an HTML page says, for a log line: its title, else the start of its visible text.
pub fn page_gist(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
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
        let tag = rest[i..].to_ascii_lowercase();
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

/// `url` as a direct download may fetch it: over `https://` (a plain `http://` one is upgraded),
/// to a public host. The URLs come from OpenAlex and the shared `[fulltext]`, so whoever writes
/// either mustn't make a collaborator's machine request `localhost` or the LAN.
// ponytail: a name that resolves to a private address still gets through; only literals are refused.
pub fn direct_url(url: &str) -> anyhow::Result<String> {
    let refuse = |why: &str| anyhow::anyhow!("{url}: {why}");
    let (scheme, rest) = url.split_once("://").ok_or_else(|| refuse("not a URL"))?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "https" | "http") {
        return Err(refuse("only https:// downloads"));
    }
    if url.contains(|c: char| c == '\\' || c.is_whitespace() || c.is_control()) {
        return Err(refuse("not a plain URL"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(refuse("a user name in the URL"));
    }
    if !public_host(authority) {
        return Err(refuse("not a public host"));
    }
    Ok(format!("https://{rest}"))
}

/// Whether `authority` (a host and maybe a port) names a host on the internet.
fn public_host(authority: &str) -> bool {
    if let Some(v6) = authority.strip_prefix('[') {
        let Some((ip, port)) = v6.split_once(']') else { return false };
        return (port.is_empty() || port.starts_with(':')) && ip.parse::<Ipv6Addr>().is_ok_and(public_v6);
    }
    let host = authority.split(':').next().unwrap_or_default().to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let last = host.rsplit('.').next().unwrap_or_default();
    // Resolvers read 2130706433, 127.1 and 0x7f.0.0.1 as IPv4 too, and no top-level domain is a number.
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return host.parse::<Ipv4Addr>().is_ok_and(public_v4);
    }
    // Only a local search domain resolves a dotless name.
    let local = [".localhost", ".local", ".internal", ".home.arpa"].iter().any(|s| format!(".{host}").ends_with(s));
    host.contains('.') && !local
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    // 100.64/10 is carrier-grade NAT and Tailscale; 192.0.0/24, 198.18/15 and 240/4 are reserved.
    let reserved = a == 0 || (a == 100 && b & 0xc0 == 64) || (a, b, c) == (192, 0, 0) || (a == 198 && b & 0xfe == 18) || a >= 240;
    !(reserved || ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_documentation() || ip.is_multicast())
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // An IPv4 address inside IPv6: mapped, compatible or NAT64.
    if let Some(v4) = ip.to_ipv4().or_else(|| (s[..6] == [0x64, 0xff9b, 0, 0, 0, 0]).then(|| Ipv4Addr::from(ip.to_bits() as u32))) {
        return public_v4(v4) && !ip.is_loopback() && !ip.is_unspecified();
    }
    // fc00::/7 unique local, fe80::/10 link-local, fec0::/10 site-local, 2001:db8::/32 documentation.
    let local = s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfe80 || s[0] & 0xffc0 == 0xfec0 || s[..2] == [0x2001, 0xdb8];
    !(local || ip.is_multicast())
}

/// Where a redirect from `url` to `location` goes; `None` for one that isn't a URL.
pub fn redirect(url: &str, location: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (origin, path) = (&url[..scheme.len() + 3 + split], &rest[split..]);
    if location.split_once("://").is_some_and(|(s, _)| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))) {
        return Some(location.to_string());
    }
    Some(match location {
        "" => return None,
        l if l.starts_with("//") => format!("{scheme}:{l}"),
        l if l.starts_with('/') => format!("{origin}{l}"),
        l => {
            let path = path.split(['?', '#']).next().unwrap_or_default();
            format!("{origin}{}/{l}", &path[..path.rfind('/').unwrap_or(0)])
        }
    })
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
        assert_eq!(page_gist("<title>K İstanbul</title>"), "K İstanbul");
        assert_eq!(page_gist("<p>İİİ<SCRIPT>x</SCRIPT>ok</p>"), "İİİ ok");
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
    fn downloads_only_from_public_hosts() {
        assert_eq!(direct_url("https://arxiv.org/pdf/1706.03762").unwrap(), "https://arxiv.org/pdf/1706.03762");
        assert_eq!(direct_url("http://eprints.example.ac.uk/1/a.pdf").unwrap(), "https://eprints.example.ac.uk/1/a.pdf");
        for ok in ["HTTPS://Link.Springer.com/x", "https://8.8.8.8/x.pdf", "https://[2606:4700::1111]:443/x", "https://example.com.:8443/x?y=1", "https://cafe.example.cafe/"] {
            assert!(direct_url(ok).is_ok(), "{ok}");
        }
        let bad = [
            "ftp://example.com/x.pdf", "file:///etc/passwd", "example.com/x.pdf", "", "https:///x", "https://me:pw@example.com/x", "https://example.com\\@evil/",
            "http://localhost/x", "https://LOCALHOST./x", "https://a.localhost/x", "https://printer.local/x", "https://desktop:11434/x", "https://metadata.google.internal/",
            "http://127.0.0.1/x", "https://127.1/", "https://2130706433/", "https://0x7f000001/", "https://0x7f.0.0.1/", "https://017700000001/", "https://010.0.0.1/",
            "https://10.0.0.1/", "https://192.168.1.1:8080/", "https://172.16.0.1/", "https://169.254.169.254/latest/meta-data/", "https://100.96.3.100/", "https://0.0.0.0/",
            "https://[::1]/", "https://[::]/", "https://[fe80::1]/", "https://[fd00::1]/", "https://[::ffff:127.0.0.1]/", "https://[::ffff:10.0.0.1]/", "https://[64:ff9b::a00:1]/",
            "https://[::1", "https://[::1]x/",
        ];
        for bad in bad {
            assert!(direct_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn follows_redirects() {
        let from = "https://a.org/doi/pdf/10.1/x?y=1";
        assert_eq!(redirect(from, "https://b.org/x.pdf").as_deref(), Some("https://b.org/x.pdf"));
        assert_eq!(redirect(from, "//b.org/x.pdf").as_deref(), Some("https://b.org/x.pdf"));
        assert_eq!(redirect(from, "/x.pdf").as_deref(), Some("https://a.org/x.pdf"));
        assert_eq!(redirect(from, "x.pdf").as_deref(), Some("https://a.org/doi/pdf/10.1/x.pdf"));
        assert_eq!(redirect("https://a.org?y=/z", "x.pdf").as_deref(), Some("https://a.org/x.pdf"));
        assert_eq!(redirect(from, "http://127.0.0.1/").as_deref(), Some("http://127.0.0.1/"));
        assert_eq!(redirect(from, ""), None);
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
