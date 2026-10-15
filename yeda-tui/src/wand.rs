use crate::library::Ref;
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum Id {
    Doi(String),
    Arxiv(String),
    Isbn(String),
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

fn strip_version(id: &str) -> &str {
    match id.rsplit_once('v') {
        Some((base, v)) if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => base,
        _ => id,
    }
}

fn is_arxiv(id: &str) -> bool {
    let digits = |s: &str, lens: &[usize]| lens.contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    match (id.split_once('.'), id.split_once('/')) {
        (Some((a, b)), None) => digits(a, &[4]) && digits(b, &[4, 5]),
        (_, Some((archive, n))) => {
            !archive.is_empty()
                && archive.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '.')
                && digits(n, &[7])
        }
        _ => false,
    }
}

pub fn parse_id(input: &str) -> Option<Id> {
    let s = input.trim();
    for p in ["https://doi.org/", "http://doi.org/", "https://dx.doi.org/", "doi.org/", "doi:"] {
        if let Some(rest) = strip_prefix_ci(s, p) {
            return Some(Id::Doi(rest.trim().into()));
        }
    }
    for p in ["https://arxiv.org/abs/", "https://arxiv.org/pdf/", "arxiv.org/abs/", "arxiv:"] {
        if let Some(rest) = strip_prefix_ci(s, p) {
            return Some(Id::Arxiv(strip_version(rest.trim().trim_end_matches(".pdf")).into()));
        }
    }
    if s.starts_with("10.") && s.contains('/') {
        return Some(Id::Doi(s.into()));
    }
    if is_arxiv(strip_version(s)) {
        return Some(Id::Arxiv(strip_version(s).into()));
    }
    let isbn: String = strip_prefix_ci(s, "isbn")
        .unwrap_or(s)
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | ':'))
        .collect::<String>()
        .to_uppercase();
    let valid = match isbn.len() {
        10 => isbn[..9].chars().all(|c| c.is_ascii_digit()) && isbn.ends_with(|c: char| c.is_ascii_digit() || c == 'X'),
        13 => isbn.chars().all(|c| c.is_ascii_digit()),
        _ => false,
    };
    valid.then_some(Id::Isbn(isbn))
}

// ponytail: blocking request freezes the UI for up to the timeout; move to a thread if that bites.
pub fn fetch(input: &str) -> Result<Ref> {
    let id = parse_id(input).context("not a DOI, arXiv ID or ISBN")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let get = |url: &str| -> Result<Value> {
        let body = agent
            .get(url)
            .header("Accept", "application/vnd.citationstyles.csl+json, application/json")
            .call()
            .with_context(|| url.to_string())?
            .body_mut()
            .read_to_string()?;
        Ok(serde_json::from_str(&body)?)
    };
    // doi.org content negotiation returns CSL-JSON for Crossref DOIs and for arXiv's DataCite DOIs alike.
    Ok(match id {
        Id::Doi(doi) => from_csl(&get(&format!("https://doi.org/{doi}"))?),
        Id::Arxiv(a) => {
            let mut r = from_csl(&get(&format!("https://doi.org/10.48550/arXiv.{a}"))?);
            r.extra.insert("eprint".into(), toml::Value::String(a));
            r
        }
        Id::Isbn(isbn) => {
            let v = get(&format!(
                "https://openlibrary.org/api/books?bibkeys=ISBN:{isbn}&format=json&jscmd=data"
            ))?;
            let mut r = from_openlibrary(v.get(format!("ISBN:{isbn}")).context("ISBN not found on OpenLibrary")?);
            r.extra.insert("isbn".into(), toml::Value::String(isbn));
            r
        }
    })
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.split_whitespace().collect::<Vec<_>>().join(" ")),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => a.first().and_then(text),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn csl_name(a: &Value) -> Option<String> {
    match (a["family"].as_str(), a["given"].as_str()) {
        (Some(f), Some(g)) => Some(format!("{f}, {g}")),
        (Some(f), None) => Some(f.into()),
        _ => a["literal"].as_str().map(Into::into),
    }
}

fn from_csl(v: &Value) -> Ref {
    let kind = match v["type"].as_str().unwrap_or("") {
        "journal-article" | "article-journal" | "article" => "article",
        "paper-conference" | "proceedings-article" => "inproceedings",
        "book" | "monograph" => "book",
        "chapter" | "book-chapter" => "incollection",
        "thesis" | "dissertation" => "thesis",
        "report" => "report",
        _ => "misc",
    };
    let year = ["issued", "published-print", "published-online"].iter().find_map(|k| {
        let y = &v[k]["date-parts"][0][0];
        y.as_i64().or_else(|| y.as_str()?.parse().ok())
    });
    let mut r = Ref {
        kind: kind.into(),
        title: text(&v["title"]).unwrap_or_default(),
        authors: v["author"].as_array().into_iter().flatten().filter_map(csl_name).collect(),
        year: year.map(|y| y as i32),
        doi: text(&v["DOI"]).map(|d| d.to_lowercase()),
        ..Default::default()
    };
    for (csl, bib) in [
        ("container-title", "journal"),
        ("volume", "volume"),
        ("issue", "number"),
        ("page", "pages"),
        ("publisher", "publisher"),
        ("URL", "url"),
    ] {
        if let Some(s) = text(&v[csl]) {
            r.extra.insert(bib.into(), toml::Value::String(s));
        }
    }
    r
}

fn from_openlibrary(v: &Value) -> Ref {
    let title = match (text(&v["title"]), text(&v["subtitle"])) {
        (Some(t), Some(s)) => format!("{t}: {s}"),
        (t, _) => t.unwrap_or_default(),
    };
    let authors = v["authors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .map(|n| match n.rsplit_once(' ') {
            Some((given, family)) => format!("{family}, {given}"),
            None => n.into(),
        })
        .collect();
    let year = v["publish_date"]
        .as_str()
        .and_then(|d| d.split(|c: char| !c.is_ascii_digit()).find(|t| t.len() == 4)?.parse().ok());
    let mut r = Ref { kind: "book".into(), title, authors, year, ..Default::default() };
    if let Some(p) = text(&v["publishers"][0]["name"]) {
        r.extra.insert("publisher".into(), toml::Value::String(p));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_identifiers() {
        let doi = |s: &str| Some(Id::Doi(s.into()));
        let arxiv = |s: &str| Some(Id::Arxiv(s.into()));
        assert_eq!(parse_id("10.1038/nature14539"), doi("10.1038/nature14539"));
        assert_eq!(parse_id("https://doi.org/10.1038/x"), doi("10.1038/x"));
        assert_eq!(parse_id("DOI: 10.1/y"), doi("10.1/y"));
        assert_eq!(parse_id("1706.03762v5"), arxiv("1706.03762"));
        assert_eq!(parse_id("arXiv:1706.03762"), arxiv("1706.03762"));
        assert_eq!(parse_id("https://arxiv.org/pdf/2101.00001v2.pdf"), arxiv("2101.00001"));
        assert_eq!(parse_id("hep-th/9901001"), arxiv("hep-th/9901001"));
        assert_eq!(parse_id("978-0-262-03384-8"), Some(Id::Isbn("9780262033848".into())));
        assert_eq!(parse_id("ISBN 0-306-40615-x"), Some(Id::Isbn("030640615X".into())));
        assert_eq!(parse_id("hello world"), None);
        assert_eq!(parse_id("12345"), None);
    }

    #[test]
    fn maps_csl() {
        let r = from_csl(&json!({
            "type": "journal-article",
            "title": "Deep\n  learning",
            "author": [{"family": "LeCun", "given": "Yann"}, {"literal": "The Team"}],
            "issued": {"date-parts": [[2015, 5]]},
            "DOI": "10.1038/NATURE14539",
            "container-title": ["Nature"],
            "volume": "521"
        }));
        assert_eq!(r.kind, "article");
        assert_eq!(r.title, "Deep learning");
        assert_eq!(r.authors, vec!["LeCun, Yann", "The Team"]);
        assert_eq!(r.year, Some(2015));
        assert_eq!(r.doi.as_deref(), Some("10.1038/nature14539"));
        assert_eq!(r.extra["journal"].as_str(), Some("Nature"));
    }

    #[test]
    fn maps_openlibrary() {
        let r = from_openlibrary(&json!({
            "title": "Introduction to Algorithms",
            "authors": [{"name": "Thomas H. Cormen"}],
            "publish_date": "March 5, 2009",
            "publishers": [{"name": "The MIT Press"}]
        }));
        assert_eq!(r.authors, vec!["Cormen, Thomas H."]);
        assert_eq!(r.year, Some(2009));
        assert_eq!(r.extra["publisher"].as_str(), Some("The MIT Press"));
    }
}
