use anyhow::Result;
use std::sync::mpsc::{self, Receiver};
use yeda_core::discover::{self, Found};
use yeda_core::graph::{self, Work};
use yeda_core::library::Ref;
use yeda_core::llm::Profile;

/// A suggested paper, with the work OpenAlex has for it (by DOI, else by title) if any.
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
}

/// Asks the LLM in the background, then looks each suggestion up in OpenAlex.
pub fn spawn(profile: Profile, key: Option<String>, prompt: &str, from: &[&Ref], openalex_key: Option<String>) -> Receiver<Result<Vec<Candidate>>> {
    let text = discover::request(prompt, from);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let found = yeda_net::llm::search(&profile, discover::SYSTEM, &text, key.as_deref()).and_then(|answer| discover::parse(&answer));
        let _ = tx.send(found.map(|found| {
            let client = yeda_net::openalex::Client::new(openalex_key);
            found.into_iter().map(|f| Candidate { work: look_up(&client, &f), found: f }).collect()
        }));
    });
    rx
}

/// A suggestion the index can't find (a failed request counts as not found) may not exist.
fn look_up(client: &yeda_net::openalex::Client, f: &Found) -> Option<Work> {
    let by_doi = f.doi.as_deref().and_then(|d| client.one_by_doi(d).ok().flatten()).filter(|w| graph::same_title(&w.title, &f.title));
    let by_title = |year| client.by_title(&f.title, year).ok()?.into_iter().find(|w| graph::same_title(&w.title, &f.title));
    // OpenAlex's year can differ from the paper's, as for a preprint.
    by_doi.or_else(|| by_title(f.year)).or_else(|| f.year.and_then(|_| by_title(None)))
}
