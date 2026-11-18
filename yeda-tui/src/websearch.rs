//! `:setup web-search`: the provider groups and the two discover limits, as one staged popup.

use std::collections::HashMap;
use yeda_core::library::Library;
use yeda_core::web_search::{Provider, Settings};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Header(&'static str),
    Provider(Provider),
    Queries,
    Results,
}

pub struct WebSearchForm {
    pub settings: Settings,
    pub sel: usize,
    key_set: HashMap<Provider, bool>,
    pub store_unavailable: bool,
}

impl WebSearchForm {
    pub fn new(lib: &Library) -> Self {
        let mut form = WebSearchForm { settings: lib.web_search.clone(), sel: 0, key_set: HashMap::new(), store_unavailable: false };
        form.refresh();
        form.sel = form.first_selectable();
        form
    }

    /// Re-reads the key state after a key or URL form closes.
    pub fn refresh(&mut self) {
        self.key_set.clear();
        self.store_unavailable = false;
        for p in [Provider::Keenable, Provider::Exa, Provider::Parallel, Provider::Firecrawl] {
            match yeda_secrets::get(account(p)) {
                Ok(secret) => {
                    self.key_set.insert(p, secret.is_some_and(|s| !s.is_empty()));
                }
                Err(_) => self.store_unavailable = true,
            }
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Header("Always · all queried, results merged")];
        rows.extend(self.settings.always.iter().map(|&p| Row::Provider(p)));
        rows.push(Row::Header("Fallback · only if every \"always\" provider fails"));
        rows.extend(self.settings.fallback.iter().map(|&p| Row::Provider(p)));
        rows.push(Row::Header("Disabled"));
        rows.extend(self.settings.disabled.iter().map(|&p| Row::Provider(p)));
        rows.push(Row::Header("Discover"));
        rows.push(Row::Queries);
        rows.push(Row::Results);
        rows
    }

    fn first_selectable(&self) -> usize {
        self.rows().iter().position(|r| !matches!(r, Row::Header(_))).unwrap_or(0)
    }

    /// `g`/`G`: the first or last row that can be selected.
    pub fn select_edge(&mut self, last: bool) {
        let rows = self.rows();
        let found = if last { rows.iter().rposition(|r| !matches!(r, Row::Header(_))) } else { rows.iter().position(|r| !matches!(r, Row::Header(_))) };
        if let Some(i) = found {
            self.sel = i;
        }
    }

    pub fn selected(&self) -> Option<Row> {
        self.rows().get(self.sel).copied().filter(|r| !matches!(r, Row::Header(_)))
    }

    pub fn move_cursor(&mut self, down: bool) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let mut i = self.sel;
        for _ in 0..rows.len() {
            i = if down { (i + 1) % rows.len() } else { (i + rows.len() - 1) % rows.len() };
            if !matches!(rows[i], Row::Header(_)) {
                self.sel = i;
                return;
            }
        }
    }

    /// Puts the selected provider in a group; the cursor follows it.
    pub fn set_group(&mut self, group: usize) -> bool {
        let Some(Row::Provider(p)) = self.selected() else { return false };
        let was_on = self.settings.is_enabled(p);
        for list in [&mut self.settings.always, &mut self.settings.fallback, &mut self.settings.disabled] {
            list.retain(|q| *q != p);
        }
        match group {
            0 => self.settings.always.push(p),
            1 => self.settings.fallback.push(p),
            _ => self.settings.disabled.push(p),
        }
        self.follow(p);
        !was_on && self.settings.is_enabled(p) && p == Provider::ModelProviderWebSearch
    }

    /// `←/→` on a provider cycles its group; on the two numbers it changes the value.
    pub fn cycle(&mut self, back: bool) {
        match self.selected() {
            Some(Row::Provider(p)) => {
                let group = if self.settings.always.contains(&p) { 0 } else if self.settings.fallback.contains(&p) { 1 } else { 2 };
                let next = if back { (group + 2) % 3 } else { (group + 1) % 3 };
                self.set_group(next);
            }
            Some(Row::Queries) => self.nudge(back),
            Some(Row::Results) => self.nudge(back),
            _ => {}
        }
    }

    fn nudge(&mut self, lower: bool) {
        match self.selected() {
            Some(Row::Queries) => {
                self.settings.discover_queries = if lower { (self.settings.discover_queries - 1).max(*yeda_core::web_search::QUERIES.start()) } else { (self.settings.discover_queries + 1).min(*yeda_core::web_search::QUERIES.end()) };
            }
            Some(Row::Results) => {
                self.settings.discover_results = if lower { (self.settings.discover_results - 1).max(*yeda_core::web_search::RESULTS.start()) } else { (self.settings.discover_results + 1).min(*yeda_core::web_search::RESULTS.end()) };
            }
            _ => {}
        }
    }

    fn follow(&mut self, p: Provider) {
        if let Some(i) = self.rows().iter().position(|r| *r == Row::Provider(p)) {
            self.sel = i;
        }
    }

    /// What the state column shows, never the URL or a mode's endpoint.
    pub fn state(&self, p: Provider) -> String {
        if crate::discover::down(p).is_some() {
            return "down until restart".into();
        }
        match p {
            Provider::Searxng => self.settings.searxng_url.as_deref().and_then(yeda_core::web_search::searxng_host).unwrap_or_else(|| "needs URL".into()),
            Provider::ModelProviderWebSearch => "LLM's own search".into(),
            _ => if self.key_set.get(&p).copied().unwrap_or(false) { "key set" } else { "keyless" }.into(),
        }
    }

    pub fn hint(&self) -> String {
        match self.selected() {
            Some(Row::Provider(p)) => {
                let down = crate::discover::down(p);
                if let Some(reason) = down {
                    return match p {
                        Provider::Searxng => format!("down ({reason}), disabled until restart; e edits the URL, and saving a new one clears it"),
                        Provider::ModelProviderWebSearch => format!("down ({reason}), disabled until restart; no form here, only a restart clears it"),
                        _ => format!("down ({reason}), disabled until restart; e sets a new key and clears it"),
                    };
                }
                match p {
                    Provider::Keenable if self.key_set.get(&p).copied().unwrap_or(false) => "own key, no hourly limit; e replaces it".into(),
                    Provider::Keenable => "shared public tier, limits per IP; e sets an API key".into(),
                    Provider::Exa if self.key_set.get(&p).copied().unwrap_or(false) => "own key, billed by Exa; e replaces it".into(),
                    Provider::Exa => "shared free tier; needs a direct IPv4 connection; e sets an API key".into(),
                    Provider::Parallel if self.key_set.get(&p).copied().unwrap_or(false) => "own key, billed by Parallel after any free usage; e replaces it".into(),
                    Provider::Parallel => "shared free tier, lower limits; e sets an API key".into(),
                    Provider::Firecrawl if self.key_set.get(&p).copied().unwrap_or(false) => "own key, higher limits; e replaces it".into(),
                    Provider::Firecrawl => "shared free tier, lower limits; e sets an API key".into(),
                    Provider::Searxng => "your own instance; https only; no API key exists; e edits the URL".into(),
                    Provider::ModelProviderWebSearch => "the LLM profile's search: anthropic, mistral, openrouter; none on openai, opencode-go; uses the LLM key".into(),
                }
            }
            Some(Row::Queries) => "queries the model writes per :discover; each is one search".into(),
            Some(Row::Results) => "papers shown".into(),
            _ => String::new(),
        }
    }

    /// Whether any enabled provider is keyless, so the shared-tier footer belongs on screen.
    pub fn keyless(&self) -> bool {
        self.settings.always.iter().chain(&self.settings.fallback).any(|p| p.has_key() && !self.key_set.get(p).copied().unwrap_or(false))
    }

    pub fn searxng_in_use(&self) -> bool {
        self.settings.always.contains(&Provider::Searxng) || self.settings.fallback.contains(&Provider::Searxng)
    }

    /// The five keys `:setup web-search` writes.
    pub fn section(&self) -> toml::Table {
        let names = |list: &[Provider]| toml::Value::Array(list.iter().map(|p| toml::Value::String(p.name().into())).collect());
        let mut t = toml::Table::new();
        t.insert("always_search_providers".into(), names(&self.settings.always));
        t.insert("fallback_search_providers".into(), names(&self.settings.fallback));
        t.insert("disabled_search_providers".into(), names(&self.settings.disabled));
        t.insert("discover_queries".into(), (self.settings.discover_queries as i64).into());
        t.insert("discover_results".into(), (self.settings.discover_results as i64).into());
        t
    }
}

/// The secret-store account of a provider's optional key.
pub fn account(provider: Provider) -> &'static str {
    match provider {
        Provider::Keenable => yeda_secrets::KEENABLE,
        Provider::Exa => yeda_secrets::EXA,
        Provider::Parallel => yeda_secrets::PARALLEL,
        Provider::Firecrawl => yeda_secrets::FIRECRAWL,
        _ => "",
    }
}

/// The provider whose key is kept under `account`, if any.
pub fn provider_of_account(account: &str) -> Option<Provider> {
    [Provider::Keenable, Provider::Exa, Provider::Parallel, Provider::Firecrawl].into_iter().find(|p| self::account(*p) == account)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> WebSearchForm {
        let lib = Library::parse(Some("[web_search]\nalways_search_providers = [\"keenable\", \"searxng\"]\nfallback_search_providers = [\"exa\"]\ndisabled_search_providers = [\"parallel\", \"firecrawl\", \"model_provider_web_search\"]\nsearxng_url = \"https://searx.example.org\"\n"), None, [], None).unwrap();
        let mut form = WebSearchForm { settings: lib.web_search.clone(), sel: 0, key_set: HashMap::new(), store_unavailable: false };
        form.sel = form.first_selectable();
        form
    }

    #[test]
    fn groups_move_and_cursor_skips_headers() {
        let mut f = form();
        assert!(matches!(f.selected(), Some(Row::Provider(Provider::Keenable))));
        f.move_cursor(true);
        f.move_cursor(true);
        assert_eq!(f.selected(), Some(Row::Provider(Provider::Exa)));
        assert!(!f.set_group(0));
        assert_eq!(f.settings.always, [Provider::Keenable, Provider::Searxng, Provider::Exa]);
        assert_eq!(f.selected(), Some(Row::Provider(Provider::Exa)));
        f.set_group(2);
        assert!(!f.settings.always.contains(&Provider::Exa));
        assert!(f.settings.disabled.contains(&Provider::Exa));
        f.sel = f.rows().iter().position(|r| *r == Row::Queries).unwrap();
        f.cycle(false);
        assert_eq!(f.settings.discover_queries, 4);
        f.settings.discover_queries = 6;
        f.cycle(false);
        assert_eq!(f.settings.discover_queries, 6);
        assert_eq!(f.state(Provider::Searxng), "searx.example.org");
        assert!(f.searxng_in_use());
        assert_eq!(f.section()["discover_queries"].as_integer(), Some(6));
    }
}
