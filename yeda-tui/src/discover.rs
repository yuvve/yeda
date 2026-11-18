use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yeda_core::discover::{self, Found, Hit, RankCandidate};
use yeda_core::graph::{self, Work};
use yeda_core::library::{Library, Ref};
use yeda_core::llm::Profile;
use yeda_core::web_search::{Provider, Settings};

/// A phase waits this long for its providers.
const PHASE: Duration = Duration::from_secs(15);
/// The most hits matched against OpenAlex, so the ranking prompt stays bounded.
const MAX_MATCHED: usize = 60;

/// A suggested paper, with the work OpenAlex has for it.
pub struct Candidate {
    pub found: Found,
    pub work: Option<Work>,
}

impl Candidate {
    /// What adds it to the library: the DOI, else the URL.
    pub fn id(&self) -> Option<String> {
        self.work.as_ref().and_then(|w| w.doi.clone()).or(self.found.doi.clone()).or(self.found.url.clone())
    }

    pub fn title(&self) -> &str {
        self.work.as_ref().map_or(&self.found.title, |w| &w.title)
    }

    /// The library's reference for it, by DOI, else by title.
    pub fn in_library<'a>(&self, lib: &'a Library) -> Option<&'a Ref> {
        let doi = self.work.as_ref().and_then(|w| w.doi.clone()).or(self.found.doi.clone());
        doi.and_then(|d| lib.find_doi(&d)).or_else(|| lib.refs.iter().find(|r| graph::same_title(&r.title, self.title())))
    }

    /// OpenAlex's first author, else what the model wrote.
    pub fn authors(&self) -> String {
        let work = self.work.as_ref().filter(|w| !w.authors.is_empty());
        work.map_or(self.found.authors.clone(), |w| format!("{}{}", graph::surname(&w.authors[0]), if w.authors.len() > 1 { " et al." } else { "" }))
    }
}

/// The suggestions, and a note on how the search went.
pub type Suggestions = Result<(Vec<Candidate>, Option<String>)>;

pub enum Msg {
    Progress(String),
    Done(Suggestions),
}

pub struct Input {
    pub profile: Profile,
    pub key: Option<String>,
    pub prompt: String,
    pub from: Vec<Ref>,
    pub openalex_key: Option<String>,
    pub settings: Settings,
    pub keys: HashMap<Provider, String>,
    /// The device cache folder (`$XDG_CACHE_HOME/yeda`), if there is one.
    pub cache: Option<PathBuf>,
    /// `:discover!`: read no cached results, but keep writing them.
    pub fresh: bool,
}

/// Runs the pipeline in the background, reporting the queries while it searches.
pub fn spawn(input: Input) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let progress = tx.clone();
        let result = run(&input, &|line| drop(progress.send(Msg::Progress(line))));
        let _ = tx.send(Msg::Done(result));
    });
    rx
}

/// Queries, search, OpenAlex matching and ranking; the same pipeline for the TUI and the CLI.
pub fn run(input: &Input, progress: &dyn Fn(String)) -> Suggestions {
    let refs: Vec<&Ref> = input.from.iter().collect();
    let query_text = discover::query_request(&input.prompt, &refs, input.settings.discover_queries);
    let answer = yeda_net::llm::text(&input.profile, discover::QUERY_SYSTEM, &query_text, input.key.as_deref())?;
    let queries = discover::parse_queries(&answer, input.settings.discover_queries)?;
    progress(format!("searching: {}", queries.join("; ")));

    let states = states();
    let arxiv_max = input.settings.discover_results * queries.len();
    let (mut outcomes, mut arxiv_rx, mut arxiv_deadline) = if input.settings.always.is_empty() {
        (Vec::new(), Some(start_arxiv(&queries, arxiv_max, input)), Instant::now() + PHASE)
    } else {
        let deadline = Instant::now() + PHASE;
        (phase(&input.settings.always, &queries, input, deadline, states), Some(start_arxiv(&queries, arxiv_max, input)), deadline)
    };

    let fallback_ran = outcomes.iter().all(|o| o.answered == 0) && !input.settings.fallback.is_empty();
    let mut fallbacks = Vec::new();
    if fallback_ran {
        let deadline = Instant::now() + PHASE;
        fallbacks = phase(&input.settings.fallback, &queries, input, deadline, states);
        if input.settings.always.is_empty() {
            arxiv_deadline = deadline;
        }
    }
    let mut hits: Vec<Hit> = outcomes.iter().flat_map(|o| o.hits.iter().cloned()).collect();
    hits.extend(fallbacks.iter().flat_map(|o| o.hits.iter().cloned()));
    let arxiv_failure = match arxiv_rx.take() {
        Some(rx) => match rx.recv_timeout(arxiv_deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(answer)) => {
                hits.extend(answer.hits);
                None
            }
            Ok(Err(failure)) => Some(failure.reason),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Some("timeout".to_string()),
        },
        None => None,
    };
    outcomes.extend(fallbacks);

    let note = search_note(&outcomes, arxiv_failure.as_deref());
    let hits = discover::dedupe_hits(hits);
    let client = yeda_net::openalex::Client::new(input.openalex_key.clone());
    let mut notes: Vec<String> = note.into_iter().collect();
    let (mut found, dropped, openalex_failure) = match_hits(&client, &hits, input);
    if let Some(reason) = openalex_failure {
        notes.push(format!("OpenAlex: {reason}"));
    }
    if found.is_empty() {
        if dropped > 0 {
            notes.push(format!("{dropped} non-academic hits dropped"));
        }
        return Ok((Vec::new(), joined(notes)));
    }
    rank(input, &mut found, &mut notes);
    found.truncate(input.settings.discover_results);
    Ok((found, joined(notes)))
}

fn joined(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn states() -> &'static States {
    static STATES: OnceLock<States> = OnceLock::new();
    STATES.get_or_init(States::default)
}

/// Why a provider got no request: it failed for a reason other than a rate limit.
pub fn down(provider: Provider) -> Option<String> {
    states().down(provider)
}

/// Forgets a provider's cool-down and down state; a key or URL change makes it a different service.
pub fn clear_state(provider: Provider) {
    states().clear(provider);
}

#[derive(Default)]
struct State {
    /// Consecutive headerless 429s.
    n: u32,
    until: Option<Instant>,
    down: Option<String>,
}

#[derive(Default)]
struct States(Mutex<HashMap<Provider, State>>);

impl States {
    /// Why the provider gets no request now: its down reason, while `until` has not passed, or none.
    fn blocked(&self, provider: Provider) -> Option<(String, bool)> {
        let states = self.0.lock().unwrap();
        let state = states.get(&provider)?;
        if let Some(reason) = &state.down {
            return Some((reason.clone(), true));
        }
        state.until.filter(|u| *u > Instant::now()).map(|_| ("rate limited".to_string(), false))
    }

    fn clear(&self, provider: Provider) {
        self.0.lock().unwrap().remove(&provider);
    }

    fn down(&self, provider: Provider) -> Option<String> {
        self.0.lock().unwrap().get(&provider).and_then(|s| s.down.clone())
    }

    fn success(&self, provider: Provider, reset_in: Option<Duration>) {
        let mut states = self.0.lock().unwrap();
        let state = states.entry(provider).or_default();
        state.n = 0;
        state.down = None;
        state.until = reset_in.map(|d| Instant::now() + d);
    }

    fn failure(&self, provider: Provider, failure: &yeda_net::web_search::Failure) {
        let mut states = self.0.lock().unwrap();
        let state = states.entry(provider).or_default();
        match failure.kind {
            yeda_net::web_search::Kind::Setting => {}
            yeda_net::web_search::Kind::Down => {
                state.down = Some(failure.reason.clone());
                state.until = None;
            }
            yeda_net::web_search::Kind::RateLimited { wait } => {
                let now = Instant::now();
                let cooling = state.until.is_some_and(|u| u > now);
                match wait {
                    Some(wait) => state.until = Some(now + wait),
                    None => {
                        if !cooling {
                            state.n += 1;
                        }
                        let ceil = 600u64.min(10u64.saturating_mul(1u64 << (state.n.saturating_sub(1)).min(6)));
                        let floor = 5u64.min(ceil);
                        let jitter = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::from(d.subsec_nanos())) % (ceil - floor + 1);
                        state.until = Some(now + Duration::from_secs(floor + jitter));
                    }
                }
            }
        }
    }
}

struct Outcome {
    provider: Provider,
    expected: usize,
    answered: usize,
    /// How many of the answers came from the cache.
    cached: usize,
    hits: Vec<Hit>,
    /// The first failure's reason.
    reason: Option<String>,
    down: bool,
    done: bool,
}

fn cache_key(provider: Provider, keyed: bool, searxng: Option<&str>, query: &str, limit: usize) -> String {
    let searxng = searxng.and_then(yeda_core::web_search::searxng_host).unwrap_or_default();
    format!("{}|{keyed}|{searxng}|{limit}|{query}", provider.name())
}

fn phase(providers: &[Provider], queries: &[String], input: &Input, deadline: Instant, states: &States) -> Vec<Outcome> {
    let (tx, rx) = mpsc::channel::<(Provider, Result<yeda_net::web_search::Answer, yeda_net::web_search::Failure>)>();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    for &provider in providers {
        let outcome = Outcome { provider, expected: queries.len(), answered: 0, cached: 0, hits: Vec::new(), reason: None, down: false, done: false };
        let index = outcomes.len();
        outcomes.push(outcome);
        if let Some((reason, down)) = states.blocked(provider) {
            outcomes[index].reason = Some(reason);
            outcomes[index].down = down;
            outcomes[index].done = true;
            continue;
        }
        for query in queries {
            let key = input.keys.get(&provider).cloned();
            let searxng = input.settings.searxng_url.clone();
            let limit = input.settings.discover_results;
            let cache_key = cache_key(provider, key.is_some(), searxng.as_deref(), query, limit);
            if !input.fresh
                && let Some(body) = yeda_fs::discover::get(input.cache.as_deref(), "search", &cache_key)
                && let Ok(hits) = serde_json::from_str::<Vec<Hit>>(&body)
            {
                outcomes[index].answered += 1;
                outcomes[index].cached += 1;
                outcomes[index].hits.extend(hits);
                continue;
            }
            let tx = tx.clone();
            let query = query.clone();
            let profile = input.profile.clone();
            let llm_key = input.key.clone();
            let cache = input.cache.clone();
            std::thread::spawn(move || {
                let result = if provider == Provider::ModelProviderWebSearch {
                    native(&profile, llm_key.as_deref(), &query, limit)
                } else {
                    yeda_net::web_search::search(&yeda_net::web_search::Search { provider, query: &query, limit, key: key.as_deref(), searxng_url: searxng.as_deref() })
                };
                if let Ok(answer) = &result
                    && let Ok(body) = serde_json::to_string(&answer.hits)
                {
                    yeda_fs::discover::put(cache.as_deref(), "search", &cache_key, &body);
                }
                let _ = tx.send((provider, result));
            });
            pending.push(index);
        }
        if outcomes[index].answered >= outcomes[index].expected {
            outcomes[index].done = true;
        }
    }
    drop(tx);
    while pending.iter().any(|i| !outcomes[*i].done) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok((provider, result)) = rx.recv_timeout(left) else { break };
        let Some(index) = outcomes.iter().position(|o| o.provider == provider) else { continue };
        if outcomes[index].done {
            continue;
        }
        match result {
            Ok(answer) => {
                outcomes[index].answered += 1;
                outcomes[index].hits.extend(answer.hits);
                states.success(provider, answer.reset_in);
            }
            Err(failure) => {
                if outcomes[index].reason.is_none() {
                    outcomes[index].reason = Some(failure.reason.clone());
                }
                states.failure(provider, &failure);
                if matches!(failure.kind, yeda_net::web_search::Kind::Down) {
                    outcomes[index].down = true;
                }
            }
        }
        if outcomes[index].down || outcomes[index].answered >= outcomes[index].expected {
            outcomes[index].done = true;
        }
    }
    let timed_out: Vec<usize> = pending.iter().copied().filter(|i| !outcomes[*i].done).collect();
    for index in timed_out {
        outcomes[index].reason.get_or_insert_with(|| "timeout".to_string());
        outcomes[index].down = true;
        outcomes[index].done = true;
        states.failure(outcomes[index].provider, &yeda_net::web_search::Failure::down("timeout"));
    }
    outcomes
}

/// The provider's own web search: the model answers, and yeda keeps the links in it.
fn native(profile: &Profile, key: Option<&str>, query: &str, limit: usize) -> std::result::Result<yeda_net::web_search::Answer, yeda_net::web_search::Failure> {
    // shortcut: a native search runs the LLM's own request, which can outlive the phase deadline; its late answer is ignored.
    let text = yeda_net::llm::search(profile, "Search the web and answer with the relevant pages.", &format!("Find up to {limit} research papers about: {query}"), key).map_err(|e| yeda_net::web_search::Failure::down(format!("{e:#}")))?;
    Ok(yeda_net::web_search::Answer { hits: discover::links_in_answer(&text.text), reset_in: None })
}

fn start_arxiv(queries: &[String], max: usize, input: &Input) -> Receiver<std::result::Result<yeda_net::web_search::Answer, yeda_net::web_search::Failure>> {
    let (tx, rx) = mpsc::channel();
    let query = discover::arxiv_query(queries);
    let dir = input.cache.as_ref().map(|c| c.join("discover/arxiv"));
    std::thread::spawn(move || {
        let result = match yeda_fs::discover::spaced(dir.as_deref(), Duration::from_secs(3), || yeda_net::web_search::arxiv(&query, max)) {
            Ok(result) => result,
            Err(e) => Err(yeda_net::web_search::Failure::down(format!("{e:#}"))),
        };
        let _ = tx.send(result);
    });
    rx
}

fn search_note(outcomes: &[Outcome], arxiv_failure: Option<&str>) -> Option<String> {
    let served: Vec<String> = outcomes
        .iter()
        .filter(|o| o.answered > 0)
        .map(|o| {
            let failed = o.expected.saturating_sub(o.answered);
            let cached = if o.cached == o.answered && o.cached > 0 { " (cached)" } else { "" };
            if failed == 0 {
                format!("{}{cached}", o.provider.name())
            } else {
                format!("{} ({failed} of {} failed: {})", o.provider.name(), o.expected, reason_text(o))
            }
        })
        .collect();
    let mut failed: Vec<String> = outcomes.iter().filter(|o| o.answered == 0).map(|o| format!("{}: {}", o.provider.name(), reason_text(o))).collect();
    if let Some(reason) = arxiv_failure {
        failed.push(format!("arXiv: {reason}"));
    }
    let tail = (!failed.is_empty()).then(|| failed.join("; "));
    let note = if served.is_empty() {
        if outcomes.is_empty() {
            match tail {
                Some(tail) => format!("web search is off; arXiv only ({tail})"),
                None => "web search is off; arXiv only".to_string(),
            }
        } else {
            format!("web search failed ({}); continuing with arXiv only", tail.unwrap_or_default())
        }
    } else {
        match tail {
            Some(tail) => format!("web search: {} ({tail})", served.join(", ")),
            None => format!("web search: {}", served.join(", ")),
        }
    };
    Some(note)
}

fn reason_text(o: &Outcome) -> String {
    let reason = o.reason.clone().unwrap_or_else(|| "failed".into());
    if o.down { format!("down ({reason}), disabled until restart") } else { reason }
}

/// Matches hits to OpenAlex works, deduplicated by work; hits without a confident match are dropped.
fn match_hits(client: &yeda_net::openalex::Client, hits: &[Hit], input: &Input) -> (Vec<Candidate>, usize, Option<String>) {
    let mut found: Vec<Candidate> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut dropped = 0;
    let mut error = None;
    for hit in hits.iter().take(MAX_MATCHED) {
        match match_hit(client, hit, input.cache.as_deref(), input.fresh) {
            Ok(Some(work)) if seen.insert(work.id.clone()) => {
                let found_hit = Found {
                    title: work.title.clone(),
                    authors: work.authors.first().map(|a| format!("{}{}", graph::surname(a), if work.authors.len() > 1 { " et al." } else { "" })).unwrap_or_default(),
                    year: work.year,
                    doi: work.doi.clone(),
                    url: hit.url.clone(),
                    why: String::new(),
                };
                found.push(Candidate { found: found_hit, work: Some(work) });
            }
            Ok(_) => dropped += 1,
            Err(e) => {
                error.get_or_insert_with(|| format!("{e:#}"));
                dropped += 1;
            }
        }
    }
    (found, dropped, error)
}

fn match_hit(client: &yeda_net::openalex::Client, hit: &Hit, cache: Option<&Path>, fresh: bool) -> Result<Option<Work>> {
    let read = (!fresh).then_some(cache).flatten();
    let doi = hit.doi.clone().or_else(|| hit.arxiv.as_ref().map(|a| format!("10.48550/arxiv.{a}")));
    if let Some(doi) = doi {
        let key = format!("doi:{doi}");
        if let Some(cached) = cached_work(read, &key) {
            return Ok(cached);
        }
        let work = client.one_by_doi(&doi)?;
        put_work(cache, &key, &work);
        if work.is_some() {
            return Ok(work);
        }
    }
    if hit.title.trim().is_empty() {
        return Ok(None);
    }
    let key = format!("title:{}", hit.title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase());
    if let Some(cached) = cached_work(read, &key) {
        return Ok(cached);
    }
    let work = client.by_title(&hit.title, None)?.into_iter().find(|w| graph::same_title(&w.title, &hit.title));
    put_work(cache, &key, &work);
    Ok(work)
}

fn cached_work(read: Option<&Path>, key: &str) -> Option<Option<Work>> {
    let body = yeda_fs::discover::get(read, "openalex", key)?;
    serde_json::from_str(&body).ok()
}

fn put_work(cache: Option<&Path>, key: &str, work: &Option<Work>) {
    if let Ok(body) = serde_json::to_string(work) {
        yeda_fs::discover::put(cache, "openalex", key, &body);
    }
}

fn rank(input: &Input, found: &mut Vec<Candidate>, notes: &mut Vec<String>) {
    let ids: Vec<String> = (1..=found.len()).map(discover::candidate_id).collect();
    let candidates: Vec<RankCandidate> = found
        .iter()
        .zip(&ids)
        .map(|(c, id)| RankCandidate {
            id: id.clone(),
            title: c.found.title.clone(),
            authors: c.authors(),
            year: c.found.year,
            venue: c.work.as_ref().and_then(|w| w.venue.clone()),
            doi: c.found.doi.clone(),
            url: c.found.url.clone(),
            abstract_text: None,
        })
        .collect();
    let picks = yeda_net::llm::text(&input.profile, discover::RANK_SYSTEM, &discover::ranking_request(&input.prompt, &candidates, input.settings.discover_results), input.key.as_deref())
        .ok()
        .map(|answer| discover::parse_ranking(&answer, &ids))
        .filter(|picks| !picks.is_empty());
    let (picks, fallback) = match picks {
        Some(picks) => (picks, false),
        None => (ids.iter().map(|id| (id.clone(), String::new())).collect(), true),
    };
    if fallback {
        notes.push("ranking failed; showing the best matches".into());
    }
    let mut by_id: HashMap<String, Candidate> = ids.into_iter().zip(std::mem::take(found)).collect();
    for (id, why) in picks {
        if let Some(mut c) = by_id.remove(&id) {
            c.found.why = why;
            found.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yeda_net::web_search::Failure;

    fn outcome(provider: Provider, expected: usize, answered: usize, reason: Option<&str>, down: bool) -> Outcome {
        Outcome { provider, expected, answered, cached: 0, hits: Vec::new(), reason: reason.map(String::from), down, done: true }
    }

    #[test]
    fn headerless_limits_back_off_and_success_resets() {
        let states = States::default();
        let p = Provider::Keenable;
        states.failure(p, &Failure::rate_limited(None));
        assert_eq!(states.0.lock().unwrap()[&p].n, 1);
        assert!(states.blocked(p).is_some());
        states.success(p, None);
        assert!(states.blocked(p).is_none());
        assert_eq!(states.0.lock().unwrap()[&p].n, 0);
        states.failure(p, &Failure::rate_limited(Some(Duration::from_secs(60))));
        assert_eq!(states.0.lock().unwrap()[&p].n, 0, "a header's wait does not touch n");
        assert!(states.blocked(p).is_some());
    }

    #[test]
    fn a_down_provider_is_skipped_and_a_setting_is_not_down() {
        let states = States::default();
        let p = Provider::Exa;
        states.failure(p, &Failure::down("timeout"));
        assert_eq!(states.blocked(p), Some(("timeout".into(), true)));
        states.failure(Provider::Searxng, &Failure::setting("no URL"));
        assert_eq!(states.blocked(Provider::Searxng), None);
    }

    #[test]
    fn notes_name_served_partial_and_down_providers() {
        let note = search_note(&[outcome(Provider::Keenable, 3, 3, None, false), outcome(Provider::Exa, 3, 0, Some("timeout"), true)], None).unwrap();
        assert_eq!(note, "web search: keenable (exa: down (timeout), disabled until restart)");
        let note = search_note(&[outcome(Provider::Keenable, 3, 2, Some("rate limited"), false)], Some("rate limited")).unwrap();
        assert_eq!(note, "web search: keenable (1 of 3 failed: rate limited) (arXiv: rate limited)");
        let note = search_note(&[], None).unwrap();
        assert_eq!(note, "web search is off; arXiv only");
        let note = search_note(&[outcome(Provider::Keenable, 3, 0, Some("rate limited"), false)], None).unwrap();
        assert_eq!(note, "web search failed (keenable: rate limited); continuing with arXiv only");
        let mut cached = outcome(Provider::Keenable, 2, 2, None, false);
        cached.cached = 2;
        assert_eq!(search_note(&[cached], None).unwrap(), "web search: keenable (cached)");
        assert!(cache_key(Provider::Exa, false, None, "q", 15) != cache_key(Provider::Exa, true, None, "q", 15));
    }
}
