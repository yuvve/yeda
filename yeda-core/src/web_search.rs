//! `[web_search]`: the providers `:discover` searches, its limits, and the SearXNG instance.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Every provider name, in the order the setup popup lists them for their group.
pub const NAMES: [&str; 6] = ["keenable", "exa", "parallel", "firecrawl", "searxng", "model_provider_web_search"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Keenable,
    Exa,
    Parallel,
    Firecrawl,
    Searxng,
    ModelProviderWebSearch,
}

impl Provider {
    pub const DEFAULT_ALWAYS: [Provider; 1] = [Provider::Keenable];
    pub const DEFAULT_FALLBACK: [Provider; 3] = [Provider::Exa, Provider::Parallel, Provider::Firecrawl];
    pub const DEFAULT_DISABLED: [Provider; 2] = [Provider::Searxng, Provider::ModelProviderWebSearch];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Keenable => "keenable",
            Provider::Exa => "exa",
            Provider::Parallel => "parallel",
            Provider::Firecrawl => "firecrawl",
            Provider::Searxng => "searxng",
            Provider::ModelProviderWebSearch => "model_provider_web_search",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        NAMES.iter().find(|n| **n == name).map(|_| match name {
            "keenable" => Provider::Keenable,
            "exa" => Provider::Exa,
            "parallel" => Provider::Parallel,
            "firecrawl" => Provider::Firecrawl,
            "searxng" => Provider::Searxng,
            _ => Provider::ModelProviderWebSearch,
        })
    }

    /// Whether the provider takes an API key of its own.
    pub fn has_key(self) -> bool {
        matches!(self, Provider::Keenable | Provider::Exa | Provider::Parallel | Provider::Firecrawl)
    }
}

/// `[web_search]` in yeda.toml. Every provider is in exactly one list.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub always: Vec<Provider>,
    pub fallback: Vec<Provider>,
    pub disabled: Vec<Provider>,
    /// Queries the model writes per `:discover`.
    pub discover_queries: usize,
    /// Papers shown.
    pub discover_results: usize,
    pub searxng_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            always: Provider::DEFAULT_ALWAYS.to_vec(),
            fallback: Provider::DEFAULT_FALLBACK.to_vec(),
            disabled: Provider::DEFAULT_DISABLED.to_vec(),
            discover_queries: 3,
            discover_results: 15,
            searxng_url: None,
        }
    }
}

pub const QUERIES: std::ops::RangeInclusive<usize> = 1..=6;
pub const RESULTS: std::ops::RangeInclusive<usize> = 1..=50;

impl Settings {
    /// The settings in `table`, with the defaults resolved, and the warnings to log.
    ///
    /// A list left out takes its default minus the providers the file names elsewhere, so a
    /// provider is never in two lists; one in no list is disabled.
    pub fn parse(table: Option<&toml::Table>) -> Result<(Self, Vec<String>)> {
        let table = table.cloned().unwrap_or_default();
        let mut warnings = Vec::new();
        let mut explicit = [None, None, None];
        for (i, key) in ["always_search_providers", "fallback_search_providers", "disabled_search_providers"].iter().enumerate() {
            explicit[i] = list(&table, key)?;
        }
        for i in 0..3 {
            if let Some(names) = &explicit[i] {
                for (j, other) in names.iter().enumerate() {
                    if names[..j].contains(other) {
                        bail!("`web_search`: {} names {} twice", LIST_KEYS[i], other.name());
                    }
                    if explicit.iter().enumerate().any(|(k, l)| k != i && l.as_ref().is_some_and(|l| l.contains(other))) {
                        let other_key = (0..3).find(|k| *k != i && explicit[*k].as_ref().is_some_and(|l| l.contains(other))).unwrap_or(0);
                        bail!("`web_search`: {} is in both {} and {}", other.name(), LIST_KEYS[i], LIST_KEYS[other_key]);
                    }
                }
            }
        }
        let named: HashSet<Provider> = explicit.iter().flatten().flatten().copied().collect();
        let lists: Vec<Vec<Provider>> = [&Provider::DEFAULT_ALWAYS[..], &Provider::DEFAULT_FALLBACK, &Provider::DEFAULT_DISABLED]
            .iter()
            .enumerate()
            .map(|(i, defaults)| match &explicit[i] {
                Some(named) => named.clone(),
                None => defaults.iter().copied().filter(|p| !named.contains(p)).collect(),
            })
            .collect();
        let (always, fallback, mut disabled) = (lists[0].clone(), lists[1].clone(), lists[2].clone());
        for p in NAMES.iter().filter_map(|n| Provider::parse(n)) {
            if !always.contains(&p) && !fallback.contains(&p) && !disabled.contains(&p) {
                disabled.push(p);
            }
        }

        let discover_queries = number(&table, "discover_queries", 3, QUERIES)?;
        let discover_results = number(&table, "discover_results", 15, RESULTS)?;

        let searxng_url = match table.get("searxng_url") {
            None => None,
            Some(v) => Some(v.as_str().ok_or_else(|| anyhow::anyhow!("`web_search`: searxng_url must be a string"))?.trim().to_string()),
        }
        .filter(|u| !u.is_empty());
        if let Some(url) = &searxng_url {
            check_searxng_url(url).map_err(|e| anyhow::anyhow!("`web_search`: searxng_url {e}"))?;
            if url.contains('?') || url.contains('#') {
                warnings.push("`web_search`: the searxng_url query string is stored in yeda.toml, which syncs through git; a token does not belong there".into());
            }
        }

        Ok((Settings { always, fallback, disabled, discover_queries, discover_results, searxng_url }, warnings))
    }

    pub fn is_enabled(&self, p: Provider) -> bool {
        self.always.contains(&p) || self.fallback.contains(&p)
    }

    /// The popup's state letters and the written file both need every list.
    pub fn lists(&self) -> [(&'static str, &[Provider]); 3] {
        [
            ("always_search_providers", &self.always),
            ("fallback_search_providers", &self.fallback),
            ("disabled_search_providers", &self.disabled),
        ]
    }
}

const LIST_KEYS: [&str; 3] = ["always_search_providers", "fallback_search_providers", "disabled_search_providers"];

fn list(table: &toml::Table, key: &str) -> Result<Option<Vec<Provider>>> {
    let Some(value) = table.get(key) else { return Ok(None) };
    let Some(items) = value.as_array() else {
        bail!("`web_search`: {key} must be an array of provider names");
    };
    let mut out = Vec::new();
    for item in items {
        let Some(name) = item.as_str() else {
            bail!("`web_search`: {key} must be an array of provider names");
        };
        let Some(p) = Provider::parse(name) else {
            bail!("`web_search`: unknown provider `{name}`; use one of {}", NAMES.join(", "));
        };
        out.push(p);
    }
    Ok(Some(out))
}

fn number(table: &toml::Table, key: &str, default: usize, range: std::ops::RangeInclusive<usize>) -> Result<usize> {
    let Some(value) = table.get(key) else { return Ok(default) };
    let n = value.as_integer().and_then(|n| usize::try_from(n).ok());
    match n {
        Some(n) if range.contains(&n) => Ok(n),
        _ => bail!("`web_search`: {key} must be {} to {}", range.start(), range.end()),
    }
}

/// Refuses a SearXNG URL that would lose a secret in the clear or carry one into git.
pub fn check_searxng_url(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("must start with https://");
    }
    let rest = &url["https://".len()..];
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        bail!("must not contain a user name or password; yeda.toml is synced through git");
    }
    Ok(())
}

/// The host of a SearXNG URL, as notes and the popup show it.
pub fn searxng_host(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    Some(rest.split(['/', '?', '#']).next()?.to_string()).filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(Settings, Vec<String>)> {
        Settings::parse(Some(&toml::from_str(text)?))
    }

    #[test]
    fn defaults_are_one_provider_per_list() {
        let (s, w) = parse("").unwrap();
        assert_eq!((s.always.as_slice(), s.fallback.as_slice(), s.disabled.as_slice()), (&Provider::DEFAULT_ALWAYS[..], &Provider::DEFAULT_FALLBACK[..], &[Provider::Searxng, Provider::ModelProviderWebSearch][..]));
        assert_eq!((s.discover_queries, s.discover_results), (3, 15));
        assert!(w.is_empty());
    }

    #[test]
    fn a_named_provider_leaves_the_defaults_and_the_rest_are_disabled() {
        let (s, _) = parse("always_search_providers = [\"exa\"]").unwrap();
        assert_eq!(s.always, [Provider::Exa]);
        assert_eq!(s.fallback, [Provider::Parallel, Provider::Firecrawl]);
        assert!(s.disabled.contains(&Provider::Keenable));
        assert!(!s.disabled.contains(&Provider::Exa));
    }

    #[test]
    fn refuses_names_twice_ranges_and_bad_urls() {
        assert!(parse("always_search_providers = [\"exa\"]\nfallback_search_providers = [\"exa\"]").unwrap_err().to_string().contains("in both"));
        assert!(parse("disabled_search_providers = [\"exa\", \"exa\"]").unwrap_err().to_string().contains("twice"));
        assert!(parse("discover_queries = 7").unwrap_err().to_string().contains("1 to 6"));
        assert!(parse("discover_results = 0").unwrap_err().to_string().contains("1 to 50"));
        assert!(parse("always_search_providers = [\"brave\"]").unwrap_err().to_string().contains("unknown provider"));
        assert!(parse("searxng_url = \"http://searx.example.org\"").unwrap_err().to_string().contains("https://"));
        assert!(parse("searxng_url = \"https://me:pw@searx.example.org\"").unwrap_err().to_string().contains("user name or password"));
        let (_, warnings) = parse("searxng_url = \"https://searx.example.org/?token=abc\"").unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(searxng_host("https://searx.example.org/search"), Some("searx.example.org".into()));
    }
}
