//! OpenAlex's works API: works by DOI or id, and the works citing one.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;
use yeda_core::graph::Work;

const API: &str = "https://api.openalex.org/works";
const SELECT: &str = "id,doi,display_name,publication_year,authorships,primary_location,referenced_works,cited_by_count";
/// Values per OR filter; OpenAlex allows 100, fewer keeps the URLs short.
const CHUNK: usize = 50;

pub struct Client {
    agent: ureq::Agent,
    key: Option<String>,
}

fn status(e: &anyhow::Error) -> Option<u16> {
    e.chain().find_map(|c| match c.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(code)) => Some(*code),
        _ => None,
    })
}

/// Whether OpenAlex refused because the daily budget is used up (HTTP 429).
pub fn is_over_budget(e: &anyhow::Error) -> bool {
    status(e) == Some(429)
}

/// Whether OpenAlex rejected the API key.
pub fn is_bad_key(e: &anyhow::Error) -> bool {
    matches!(status(e), Some(401 | 403))
}

impl Client {
    /// `key` is an OpenAlex API key; without one, requests share a small per-IP daily budget.
    pub fn new(key: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
        Client { agent, key }
    }

    fn page(&self, filter: &str, sort: Option<&str>, per_page: usize, page: usize) -> Result<Vec<Work>> {
        let mut req = self
            .agent
            .get(API)
            .query("filter", filter)
            .query("select", SELECT)
            .query("per_page", per_page.to_string())
            .query("page", page.to_string());
        if let Some(s) = sort {
            req = req.query("sort", s);
        }
        if let Some(k) = &self.key {
            req = req.query("api_key", k);
        }
        let body = req.call().context("OpenAlex")?.body_mut().read_to_string()?;
        let v: Value = serde_json::from_str(&body)?;
        Ok(v["results"].as_array().context("OpenAlex: no results")?.iter().map(parse_work).collect())
    }

    fn by(&self, field: &str, values: &[&str]) -> Result<Vec<Work>> {
        let mut out = Vec::new();
        for chunk in values.chunks(CHUNK) {
            out.extend(self.page(&format!("{field}:{}", chunk.join("|")), None, CHUNK, 1)?);
        }
        Ok(out)
    }

    pub fn by_doi(&self, dois: &[&str]) -> Result<Vec<Work>> {
        self.by("doi", dois)
    }

    /// One work by DOI; `None` if OpenAlex has none. Free, unlike list requests, and it also
    /// finds works whose main DOI is another one (an arXiv preprint published later).
    pub fn one_by_doi(&self, doi: &str) -> Result<Option<Work>> {
        let mut req = self.agent.get(format!("{API}/doi:{doi}")).query("select", SELECT);
        if let Some(k) = &self.key {
            req = req.query("api_key", k);
        }
        match req.call() {
            Ok(mut resp) => Ok(Some(parse_work(&serde_json::from_str(&resp.body_mut().read_to_string()?)?))),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e).context("OpenAlex")),
        }
    }

    pub fn by_id(&self, ids: &[&str]) -> Result<Vec<Work>> {
        self.by("openalex", ids)
    }

    /// Up to `n` works citing `id`, the most cited first.
    pub fn citing(&self, id: &str, n: usize) -> Result<Vec<Work>> {
        let mut out = Vec::new();
        for page in 1.. {
            let per_page = (n - out.len()).min(100);
            let got = self.page(&format!("cites:{id}"), Some("cited_by_count:desc"), per_page, page)?;
            let done = got.len() < per_page;
            out.extend(got);
            if done || out.len() >= n {
                break;
            }
        }
        Ok(out)
    }
}

fn short(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

fn parse_work(v: &Value) -> Work {
    let str_of = |v: &Value| v.as_str().map(String::from);
    Work {
        id: v["id"].as_str().map(short).unwrap_or_default(),
        doi: v["doi"].as_str().map(|d| d.trim_start_matches("https://doi.org/").to_lowercase()),
        title: str_of(&v["display_name"]).unwrap_or_default(),
        year: v["publication_year"].as_i64().map(|y| y as i32),
        authors: v["authorships"].as_array().into_iter().flatten().filter_map(|a| str_of(&a["author"]["display_name"])).collect(),
        venue: str_of(&v["primary_location"]["source"]["display_name"]),
        references: v["referenced_works"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(short)).collect(),
        cited_by_count: v["cited_by_count"].as_u64().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_work() {
        let v: Value = serde_json::from_str(
            r#"{"id":"https://openalex.org/W2919115771","doi":"https://doi.org/10.1038/NATURE14539","display_name":"Deep learning",
            "publication_year":2015,"authorships":[{"author":{"display_name":"Yann LeCun"}}],
            "primary_location":{"source":{"display_name":"Nature"}},"referenced_works":["https://openalex.org/W146900863"],"cited_by_count":70000}"#,
        )
        .unwrap();
        let w = parse_work(&v);
        assert_eq!((w.id.as_str(), w.doi.as_deref(), w.year), ("W2919115771", Some("10.1038/nature14539"), Some(2015)));
        assert_eq!((w.authors, w.venue, w.references), (vec!["Yann LeCun".to_string()], Some("Nature".to_string()), vec!["W146900863".to_string()]));
    }

    #[test]
    #[ignore = "network"]
    fn fetches_live() {
        let c = Client::new(None);
        let w = c.by_doi(&["10.1038/nature14539"]).unwrap();
        assert_eq!(w[0].id, "W2919115771");
        assert_eq!(c.citing("W2919115771", 3).unwrap().len(), 3);
    }
}
