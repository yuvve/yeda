use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use yeda_core::graph::{self, Graph, Known, Settings, Work};
use yeda_core::library::Ref;
use yeda_net::openalex::{self, Client};

pub struct Request {
    pub refs: Vec<Ref>,
    /// Keys of the seed references.
    pub seeds: Vec<String>,
    pub settings: Settings,
    pub cache: PathBuf,
    pub key: Option<String>,
    /// Fetch citing works again even if cached.
    pub refresh: bool,
}

pub struct Fetched {
    pub graph: Graph,
    /// `(key, OpenAlex id)` of references matched for the first time.
    pub matched: Vec<(String, String)>,
    /// Seeds OpenAlex doesn't know: no DOI, or a DOI it has no work for.
    pub unknown: Vec<String>,
}

pub enum Msg {
    Progress(String),
    Done(Result<Fetched, String>),
    OverBudget,
}

/// Fetched works and citing lists, one JSON file each, so later graphs cost no requests.
struct Cache(PathBuf);

impl Cache {
    fn path(&self, kind: &str, id: &str) -> PathBuf {
        self.0.join(kind).join(format!("{id}.json"))
    }

    fn get<T: serde::de::DeserializeOwned>(&self, kind: &str, id: &str) -> Option<T> {
        serde_json::from_slice(&std::fs::read(self.path(kind, id)).ok()?).ok()
    }

    fn put(&self, kind: &str, id: &str, value: &impl serde::Serialize) -> Result<()> {
        Ok(yeda_fs::write_atomic(&self.path(kind, id), &serde_json::to_vec(value)?)?)
    }

    fn put_works(&self, works: &[Work]) -> Result<()> {
        works.iter().try_for_each(|w| self.put("works", &w.id, w))
    }
}

pub fn spawn(req: Request) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let msg = match run(req, &tx) {
            Ok(f) => Msg::Done(Ok(f)),
            Err(e) if openalex::is_over_budget(&e) => Msg::OverBudget,
            Err(e) if openalex::is_bad_key(&e) => Msg::Done(Err("OpenAlex rejected the API key; :graph key sets another".into())),
            Err(e) => Msg::Done(Err(format!("{e:#}"))),
        };
        let _ = tx.send(msg);
    });
    rx
}

fn run(req: Request, tx: &Sender<Msg>) -> Result<Fetched> {
    let progress = |s: String| drop(tx.send(Msg::Progress(s)));
    let cache = Cache(req.cache.join("openalex"));
    let client = Client::new(req.key);
    let mut refs = req.refs;

    // ponytail: DOIs OpenAlex doesn't know are asked about again on every graph; remember misses if that adds up.
    let unmatched: Vec<(usize, String)> =
        refs.iter().enumerate().filter(|(_, r)| graph::work_id(r).is_none()).filter_map(|(i, r)| Some((i, graph::lookup_doi(r)?))).collect();
    let mut matched = Vec::new();
    if !unmatched.is_empty() {
        progress(format!("matching {} references with OpenAlex…", unmatched.len()));
        let dois: Vec<&str> = unmatched.iter().map(|(_, d)| d.as_str()).collect();
        let found = client.by_doi(&dois)?;
        cache.put_works(&found)?;
        for w in &found {
            for (i, doi) in &unmatched {
                if w.doi.as_deref() == Some(doi) && graph::work_id(&refs[*i]).is_none() {
                    graph::set_work_id(&mut refs[*i], &w.id);
                    matched.push((refs[*i].key.clone(), w.id.clone()));
                }
            }
        }
        let rest: Vec<&(usize, String)> = unmatched.iter().filter(|(i, _)| graph::work_id(&refs[*i]).is_none()).collect();
        for (n, (i, doi)) in rest.iter().enumerate() {
            progress(format!("looking up DOI {}/{}…", n + 1, rest.len()));
            if let Some(w) = client.one_by_doi(doi)? {
                cache.put_works(std::slice::from_ref(&w))?;
                graph::set_work_id(&mut refs[*i], &w.id);
                matched.push((refs[*i].key.clone(), w.id));
            }
        }
    }

    let mut works: HashMap<String, Work> = HashMap::new();
    let library_ids: Vec<String> = refs.iter().filter_map(graph::work_id).map(String::from).collect();
    load_or_fetch(&cache, &client, &library_ids, &mut works, &progress, "library works")?;

    let by_key: HashMap<&str, &Ref> = refs.iter().map(|r| (r.key.as_str(), r)).collect();
    let mut seeds = Vec::new();
    let mut unknown = Vec::new();
    for key in &req.seeds {
        match by_key.get(key.as_str()).and_then(|r| graph::work_id(r)) {
            Some(id) => seeds.push(id.to_string()),
            None => unknown.push(key.clone()),
        }
    }

    let mut citing: HashMap<String, Vec<String>> = HashMap::new();
    let todo: Vec<&String> = seeds.iter().filter(|s| req.refresh || cache.get::<Vec<String>>("citing", s).is_none()).collect();
    for (n, s) in todo.iter().enumerate() {
        progress(format!("fetching citations {}/{}…", n + 1, todo.len()));
        let found = client.citing(s, req.settings.citing)?;
        cache.put_works(&found)?;
        cache.put("citing", s, &found.iter().map(|w| &w.id).collect::<Vec<_>>())?;
    }
    for s in &seeds {
        let ids: Vec<String> = cache.get("citing", s).unwrap_or_default();
        load_or_fetch(&cache, &client, &ids, &mut works, &progress, "citing works")?;
        citing.insert(s.clone(), ids);
    }

    let candidates = graph::candidates(&refs, &seeds, &Known { works: &works, citing: &citing }, req.settings.outside);
    load_or_fetch(&cache, &client, &candidates, &mut works, &progress, "outside works")?;
    let graph = graph::build(&refs, &seeds, &Known { works: &works, citing: &citing }, req.settings.outside);
    Ok(Fetched { graph, matched, unknown })
}

/// Puts the works for `ids` into `works`, from the cache or else from OpenAlex.
fn load_or_fetch(cache: &Cache, client: &Client, ids: &[String], works: &mut HashMap<String, Work>, progress: &dyn Fn(String), what: &str) -> Result<()> {
    let mut missing = Vec::new();
    for id in ids {
        if works.contains_key(id) {
            continue;
        }
        match cache.get::<Work>("works", id) {
            Some(w) => drop(works.insert(id.clone(), w)),
            None => missing.push(id.as_str()),
        }
    }
    if !missing.is_empty() {
        progress(format!("fetching {} {what}…", missing.len()));
        let found = client.by_id(&missing)?;
        cache.put_works(&found)?;
        works.extend(found.into_iter().map(|w| (w.id.clone(), w)));
    }
    Ok(())
}
