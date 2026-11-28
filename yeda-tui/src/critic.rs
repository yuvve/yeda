use crate::{discover, ops, poppler};
use anyhow::{Result, ensure};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use yeda_core::critic::{self, Context, Reference, Unverified};
use yeda_core::discover::{Hit, normalize_doi};
use yeda_core::library::Ref;
use yeda_core::llm::{Bound, Usage};
use yeda_core::{annotations, graph};
use yeda_net::llm::{self, Prompt, Reply};
use yeda_net::{doi, openalex};

const SEARCH_PHASE: Duration = Duration::from_secs(15);

pub struct Input {
    pub profile: Bound,
    pub key: Option<String>,
    pub target: Ref,
    pub pdf: PathBuf,
    pub context: Vec<(Ref, Option<PathBuf>)>,
    pub search: discover::Input,
}

pub struct Outcome {
    pub text: String,
    pub annotations: usize,
    pub findings: usize,
    pub unverified: usize,
    pub notes: Vec<String>,
    pub usage: Usage,
}

pub enum Msg {
    Progress(String),
    Done(Result<Outcome, String>),
}

pub fn spawn(mut input: Input) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = run(&mut input, &|line| drop(tx.send(Msg::Progress(line))));
        let _ = tx.send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
    });
    rx
}

fn run(input: &mut Input, progress: &dyn Fn(String)) -> Result<Outcome> {
    let key = &input.target.key;
    let pages = annotations::word_pages(&poppler::word_boxes(&input.pdf)?);
    ensure!((0..pages.len()).any(|i| !pages.page_text(i).is_empty()), "{key}'s PDF has no text layer; :critic needs one");

    let reply = {
        let text = {
            let context: Vec<_> = input.context.iter().map(|(r, pdf)| Context { r, text: pdf.as_deref().and_then(|path| poppler::pdf_text(path).ok()) }).collect();
            critic::request(&input.target, &pages, &context)
        };
        progress(format!("critic: reviewing {key} with {}…", input.profile.profile));
        // Quotes must come from the same text we anchor against, not the provider's PDF extraction.
        let prompt = Prompt { system: critic::SYSTEM.into(), text, paper: None, schema: critic::schema() };
        complete_renewing(&input.profile, &prompt, &mut input.key)?
    };
    input.search.key = input.key.take();
    let review = critic::parse(&reply.text, &pages)?;

    progress(format!("critic: checking {} references…", review.references.len()));
    let (unverified, notes) = check_references(&review.references, &input.search);
    let annots = critic::annotate(&pages, &review, &unverified, &notes, &input.profile.model);
    let annotations = annots.iter().filter(|a| a.kind == "highlight").count();
    Ok(Outcome { text: annotations::render(annots)?, annotations, findings: review.findings.len(), unverified: unverified.len(), notes, usage: reply.usage })
}

fn complete_renewing(profile: &Bound, prompt: &Prompt, key: &mut Option<String>) -> Result<Reply> {
    ops::renew_credential(profile, key, false)?;
    match llm::complete(profile, prompt, key.as_deref()) {
        Err(e) if llm::is_unauthorized(&e) && ops::renew_credential(profile, key, true)? => llm::complete(profile, prompt, key.as_deref()),
        sent => sent,
    }
}

fn search_missing_ids(references: &[Reference], search: &discover::Input) -> Vec<Hit> {
    let queries: Vec<_> = references.iter().filter(|r| r.doi.is_none() && r.arxiv.is_none())
        .map(|r| format!("{} {}", r.title, r.first_author.as_deref().unwrap_or_default())).collect();
    let mut hits = Vec::new();
    let states = discover::states();
    for batch in queries.chunks(5) {
        let mut outcomes = discover::phase(&search.settings.always, batch, search, Instant::now() + SEARCH_PHASE, states);
        if outcomes.iter().all(|o| o.answered == 0) && !search.settings.fallback.is_empty() {
            outcomes = discover::phase(&search.settings.fallback, batch, search, Instant::now() + SEARCH_PHASE, states);
        }
        hits.extend(outcomes.into_iter().flat_map(|o| o.hits));
    }
    hits
}

fn check_references(references: &[Reference], search: &discover::Input) -> (Vec<Unverified>, Vec<String>) {
    let hits = search_missing_ids(references, search);
    let client = openalex::Client::new(search.openalex_key.clone());
    let mut unverified = Vec::new();
    let mut notes = Vec::new();
    for (reference, entry) in references.iter().enumerate() {
        let web = if entry.doi.is_none() && entry.arxiv.is_none() {
            hits.iter().find(|hit| graph::same_title(&hit.title, &entry.title))
        } else {
            None
        };
        let mut hit = Hit { title: entry.title.clone(), doi: entry.doi.clone(), arxiv: entry.arxiv.clone(), ..Hit::default() };
        if let Some(web) = web {
            hit.doi = web.doi.as_deref().and_then(normalize_doi);
            if hit.doi.is_none() {
                hit.arxiv.clone_from(&web.arxiv);
            }
        }
        match discover::match_hit(&client, &hit, search.cache.as_deref(), false) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                notes.push(if openalex::is_over_budget(&e) {
                    "References not checked: OpenAlex budget spent; it resets at midnight UTC.".into()
                } else {
                    format!("References not checked: OpenAlex: {e:#}")
                });
                break;
            }
        }
        let doi = hit.doi.or_else(|| hit.arxiv.map(|id| format!("10.48550/arxiv.{id}")));
        let reason = if let Some(doi) = doi {
            match doi::exists(&doi) {
                Ok(true) => continue,
                Ok(false) => format!("its DOI {doi} does not resolve at doi.org"),
                Err(e) => {
                    notes.push(format!("doi.org: {e:#}"));
                    continue;
                }
            }
        } else if web.is_some() {
            continue;
        } else {
            "not found in OpenAlex or a web search".into()
        };
        unverified.push(Unverified { reference, reason });
    }
    (unverified, notes)
}
