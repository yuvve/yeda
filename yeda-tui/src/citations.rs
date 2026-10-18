use anyhow::Result;
use std::collections::{HashMap, HashSet};
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
    /// Work ids of papers opened in the graph that aren't seeds yet; they become seeds too.
    pub explored: Vec<String>,
    /// Work ids of the opened papers, whose full lists come back in `Fetched::lists`.
    pub opened: Vec<String>,
}

pub struct Fetched {
    pub graph: Graph,
    /// `(key, OpenAlex id)` of references matched for the first time.
    pub matched: Vec<(String, String)>,
    /// Seeds OpenAlex doesn't know: no DOI, or a DOI it has no work for.
    pub unknown: Vec<String>,
    /// Each opened paper's references and citing works.
    pub lists: Vec<(String, Lists)>,
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

/// What OpenAlex didn't find, as `doi:…`, `title:…` and `id:…`, so it isn't asked again; `:graph!` retries.
struct Misses<'a> {
    cache: &'a Cache,
    set: HashSet<String>,
}

impl<'a> Misses<'a> {
    fn load(cache: &'a Cache, refresh: bool) -> Self {
        let set = if refresh { HashSet::new() } else { cache.get("misses", "all").unwrap_or_default() };
        Misses { cache, set }
    }

    fn has(&self, what: &str) -> bool {
        self.set.contains(what)
    }

    fn add(&mut self, what: impl IntoIterator<Item = String>) -> Result<()> {
        self.set.extend(what);
        self.cache.put("misses", "all", &self.set)
    }
}

pub fn spawn(req: Request) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let msg = match run(req, &tx) {
            Ok(f) => Msg::Done(Ok(f)),
            Err(e) if openalex::is_over_budget(&e) => Msg::OverBudget,
            Err(e) if openalex::is_bad_key(&e) => Msg::Done(Err("OpenAlex rejected the API key; :secrets sets another".into())),
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
    let mut misses = Misses::load(&cache, req.refresh);

    let unmatched: Vec<(usize, String)> = refs
        .iter()
        .enumerate()
        .filter(|(_, r)| graph::work_id(r).is_none())
        .filter_map(|(i, r)| Some((i, graph::lookup_doi(r)?)))
        .filter(|(_, doi)| !misses.has(&format!("doi:{doi}")))
        .collect();
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
            match client.one_by_doi(doi)? {
                Some(w) => {
                    cache.put_works(std::slice::from_ref(&w))?;
                    graph::set_work_id(&mut refs[*i], &w.id);
                    matched.push((refs[*i].key.clone(), w.id));
                }
                None => misses.add([format!("doi:{doi}")])?,
            }
        }
    }

    let untitled: Vec<usize> = (0..refs.len())
        .filter(|&i| graph::work_id(&refs[i]).is_none() && graph::lookup_doi(&refs[i]).is_none())
        .filter(|&i| !misses.has(&format!("title:{}", refs[i].title)))
        .collect();
    for (n, &i) in untitled.iter().enumerate() {
        progress(format!("looking up {}/{} references without a DOI by title…", n + 1, untitled.len()));
        let found = client.by_title(&refs[i].title, refs[i].year)?;
        match found.into_iter().find(|w| graph::same_title(&w.title, &refs[i].title)) {
            Some(w) => {
                cache.put_works(std::slice::from_ref(&w))?;
                graph::set_work_id(&mut refs[i], &w.id);
                matched.push((refs[i].key.clone(), w.id));
            }
            None => misses.add([format!("title:{}", refs[i].title)])?,
        }
    }

    let mut works: HashMap<String, Work> = HashMap::new();
    let library_ids: Vec<String> = refs.iter().filter_map(graph::work_id).map(String::from).collect();
    load_or_fetch(&cache, &client, &mut misses, &library_ids, &mut works, &progress, "library works")?;

    let by_key: HashMap<&str, &Ref> = refs.iter().map(|r| (r.key.as_str(), r)).collect();
    let mut seeds = Vec::new();
    let mut unknown = Vec::new();
    for key in &req.seeds {
        match by_key.get(key.as_str()).and_then(|r| graph::work_id(r)) {
            Some(id) => seeds.push(id.to_string()),
            None => unknown.push(key.clone()),
        }
    }
    for id in &req.explored {
        if !seeds.contains(id) {
            seeds.push(id.clone());
        }
    }
    load_or_fetch(&cache, &client, &mut misses, &req.explored, &mut works, &progress, "explored works")?;

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
        load_or_fetch(&cache, &client, &mut misses, &ids, &mut works, &progress, "citing works")?;
        citing.insert(s.clone(), ids);
    }

    let candidates = graph::candidates(&refs, &seeds, &Known { works: &works, citing: &citing }, req.settings.outside);
    load_or_fetch(&cache, &client, &mut misses, &candidates, &mut works, &progress, "outside works")?;
    let graph = graph::build(&refs, &seeds, &Known { works: &works, citing: &citing }, req.settings.outside);
    let mut lists = Vec::new();
    for id in &req.opened {
        let references = works.get(id).map(|w| w.references.clone()).unwrap_or_default();
        load_or_fetch(&cache, &client, &mut misses, &references, &mut works, &progress, "references")?;
        let pick = |ids: &[String]| ids.iter().filter_map(|i| works.get(i).cloned()).collect();
        lists.push((id.clone(), Lists { cites: pick(&references), cited_by: pick(citing.get(id).map_or(&[][..], Vec::as_slice)) }));
    }
    Ok(Fetched { graph, matched, unknown, lists })
}

/// All references of a work and its most cited citing works, as `Graph::extend` takes them.
pub struct Lists {
    pub cites: Vec<Work>,
    pub cited_by: Vec<Work>,
}

/// Puts the works for `ids` into `works`, from the cache or else from OpenAlex.
fn load_or_fetch(cache: &Cache, client: &Client, misses: &mut Misses, ids: &[String], works: &mut HashMap<String, Work>, progress: &dyn Fn(String), what: &str) -> Result<()> {
    let mut missing = Vec::new();
    for id in ids {
        if works.contains_key(id) || misses.has(&format!("id:{id}")) {
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
        let returned: HashSet<&str> = found.iter().map(|w| w.id.as_str()).collect();
        misses.add(missing.iter().filter(|id| !returned.contains(*id)).map(|id| format!("id:{id}")))?;
        works.extend(found.into_iter().map(|w| (w.id.clone(), w)));
    }
    Ok(())
}
