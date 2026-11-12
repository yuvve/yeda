//! Subcommands that run one TUI command without the TUI, for scripts and agents.

use crate::{citations, discover, fetch, ops, pdftext};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use yeda_core::annotations;
use yeda_core::browse::{self, Filter, Sort, SortField};
use yeda_core::clusters::Kind;
use yeda_core::library::{self, Library, Ref};
use yeda_core::search::{self, Searcher};
use yeda_core::tables;

pub const USAGE: &str = "
Commands for scripts and agents (each takes --lib <dir>; <key>… may be - to read keys from stdin, one per line):
       yeda search [query] [--saved <name>] [--sort [-]<field>] [--json]
       yeda annotations <key> [--json]
       yeda add <DOI, arXiv ID or ISBN>… [--to <collection>]
       yeda new --title <title> [--type --authors \"A; B\" --year --doi --url --journal --publisher --abstract --to <collection>]
       yeda delete <key>…
       yeda cite <key>… [--format <bibtex|ident|style|file.csl>]
       yeda export <table> [file.csv]
       yeda attach <key> <file.pdf> [--to <location>]
       yeda detach <key> [--pdf <id>]
       yeda fetch <key>… [--to <location>] [--no-window]
       yeda cache [key…]
       yeda extract <key>…
       yeda graph <key>… [--refresh | --tags | --column <column>]
       yeda discover [description] [--like <key>]… [--llm <profile>] [--json]
       yeda summarize <key>… [--table <name>] [--llm <profile>] [--overwrite]
       yeda sync";

/// Every subcommand, with its options that take a value and its flags.
const COMMANDS: &[(&str, &[&str], &[&str])] = &[
    ("search", &["saved", "sort"], &["json"]),
    ("annotations", &[], &["json"]),
    ("add", &["to"], &[]),
    ("new", &["title", "type", "authors", "year", "doi", "url", "journal", "publisher", "abstract", "to"], &[]),
    ("delete", &[], &[]),
    ("cite", &["format"], &[]),
    ("export", &[], &[]),
    ("attach", &["to"], &[]),
    ("detach", &["pdf"], &[]),
    ("fetch", &["to"], &["no-window"]),
    ("cache", &[], &[]),
    ("extract", &[], &[]),
    ("graph", &["column"], &["refresh", "tags"]),
    ("discover", &["like", "llm"], &["json"]),
    ("summarize", &["table", "llm"], &["overwrite"]),
    ("sync", &[], &[]),
];

/// `println!` that returns an error instead of panicking when stdout is closed, as by `| head`.
macro_rules! out {
    ($($arg:tt)*) => { writeln!(std::io::stdout(), $($arg)*)? };
}

struct Args {
    pos: Vec<String>,
    opts: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
}

impl Args {
    fn parse(args: &[String], valued: &[&str], flags: &[&str]) -> Result<Args> {
        let mut out = Args { pos: Vec::new(), opts: HashMap::new(), flags: HashSet::new() };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--" {
                out.pos.extend(it.by_ref().cloned());
                break;
            }
            let Some(name) = a.strip_prefix("--") else {
                out.pos.push(a.clone());
                continue;
            };
            let (name, inline) = name.split_once('=').map_or((name, None), |(n, v)| (n, Some(v.to_string())));
            if flags.contains(&name) && inline.is_none() {
                out.flags.insert(name.into());
            } else if valued.contains(&name) || name == "lib" {
                let value = inline.or_else(|| it.next().cloned()).with_context(|| format!("--{name} needs a value"))?;
                out.opts.entry(name.into()).or_default().push(value);
            } else {
                bail!("unknown option --{name}");
            }
        }
        Ok(out)
    }

    fn opt(&self, name: &str) -> Option<&str> {
        self.opts.get(name).and_then(|v| v.last()).map(String::as_str)
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.opts.get(name).cloned().unwrap_or_default()
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

/// Runs `args` if it names a subcommand; `None` if it doesn't.
pub fn run(args: &[String]) -> Option<Result<()>> {
    let (command, valued, flags) = COMMANDS.iter().find(|(c, ..)| Some(*c) == args.first().map(String::as_str))?;
    let result = Args::parse(&args[1..], valued, flags).and_then(|a| dispatch(command, a));
    let closed = |e: &anyhow::Error| e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe);
    Some(result.or_else(|e| if closed(&e) { Ok(()) } else { Err(e) }))
}

fn dispatch(command: &str, a: Args) -> Result<()> {
    let dir = crate::library_dir(a.opts.get("lib").and_then(|v| v.last()))?;
    let dir = dir.canonicalize().unwrap_or(dir);
    let mut lib = crate::load_library(&dir)?;
    for (_, e) in &lib.unreadable {
        eprintln!("left out: {e}");
    }
    match command {
        "search" => search(&lib, &a),
        "annotations" => show_annotations(&lib, &dir, &a),
        "add" => add(&mut lib, &dir, &a),
        "new" => new(&lib, &dir, &a),
        "delete" => {
            let keys = keys(&lib, &a.pos)?;
            let effects = keys.iter().map(|k| lib.delete(k)).collect::<Result<Vec<_>>>()?;
            yeda_fs::apply(&dir, effects.concat())?;
            eprintln!("deleted {}", keys.join(", "));
            Ok(())
        }
        "cite" => {
            let refs = refs(&lib, &a.pos)?;
            write!(std::io::stdout(), "{}", ops::cite_text(&lib, &dir, &refs, a.opt("format").unwrap_or_default())?.0)?;
            Ok(())
        }
        "export" => {
            let table = a.pos.first().context("name the table")?;
            let (csv, n) = tables::export_csv(&lib, table)?;
            match a.pos.get(1) {
                Some(path) => {
                    let path = ops::expand_home(path);
                    std::fs::write(&path, csv).with_context(|| path.display().to_string())?;
                    eprintln!("exported {n} rows of '{table}' to {}", path.display());
                }
                None => write!(std::io::stdout(), "{csv}")?,
            }
            Ok(())
        }
        "attach" => attach(&lib, &dir, &a),
        "detach" => detach(&lib, &dir, &a),
        "fetch" => fetch_pdfs(&mut lib, &dir, &a),
        "cache" => cache(&lib, &a),
        "extract" => {
            for key in keys(&lib, &a.pos)? {
                let r = &lib.refs[ops::ref_index(&lib, &key)?];
                let n = ops::extract_annotations(&lib, &dir, r, &mut |effects| yeda_fs::apply(&dir, effects))?;
                eprintln!("{key}: {n} annotations extracted");
            }
            Ok(())
        }
        "graph" => graph(&mut lib, &dir, &a),
        "discover" => discover(&lib, &a),
        "summarize" => summarize(&mut lib, &dir, &a),
        "sync" => {
            let report = crate::sync::run(&dir, &|line| eprintln!("{line}"))?;
            eprintln!("{report}");
            Ok(())
        }
        _ => unreachable!(),
    }
}

/// `args`, or the first word of each line of stdin when `args` is `-`; each must be a key in the library.
fn keys(lib: &Library, args: &[String]) -> Result<Vec<String>> {
    let keys: Vec<String> = match args {
        [dash] if dash == "-" => {
            let lines = std::io::stdin().lock().lines().collect::<std::io::Result<Vec<_>>>()?;
            lines.iter().filter_map(|l| l.split_whitespace().next()).map(String::from).collect()
        }
        _ => args.to_vec(),
    };
    anyhow::ensure!(!keys.is_empty(), "name at least one reference by its key");
    for k in &keys {
        ops::ref_index(lib, k)?;
    }
    Ok(keys)
}

fn refs(lib: &Library, args: &[String]) -> Result<Vec<Ref>> {
    Ok(keys(lib, args)?.iter().filter_map(|k| lib.get(k).cloned()).collect())
}

/// Shows `s` on one line of a terminal, replacing the last; `""` clears it.
fn progress(s: &str) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K{s}");
    }
}

fn ref_json(r: &Ref) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_default();
    v["key"] = r.key.clone().into();
    v["tables"] = serde_json::to_value(&r.tables).unwrap_or_default();
    v
}

fn print_json(v: &Value) -> Result<()> {
    out!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn short_authors(authors: &[String]) -> String {
    match authors {
        [] => String::new(),
        [one] => one.clone(),
        [first, ..] => format!("{first} et al."),
    }
}

/// `-year` sorts by year, newest first; `field` alone ascending.
fn parse_sort(s: &str) -> Result<Sort> {
    let (descending, field) = s.strip_prefix('-').map_or((false, s), |f| (true, f));
    let field = SortField::ALL
        .into_iter()
        .find(|f| serde_json::to_value(f).ok().and_then(|v| v.as_str().map(|n| n == field)).unwrap_or(false))
        .with_context(|| format!("sort by key, title, author, year, status or type, not '{field}'"))?;
    Ok(Sort { field, descending })
}

fn search(lib: &Library, a: &Args) -> Result<()> {
    let query = a.pos.join(" ");
    let (filter, mut sort) = match a.opt("saved") {
        Some(name) => {
            let saved = lib.searches.get(name).with_context(|| format!("no saved search named '{name}'"))?;
            (Filter::Search(name.into(), saved.query.clone()), saved.order().unwrap_or_default())
        }
        None => (Filter::All, Sort::default()),
    };
    if let Some(s) = a.opt("sort") {
        sort = parse_sort(s)?;
    }
    let mut searcher = Searcher::new();
    let uses_text = [query.as_str(), if let Filter::Search(_, q) = &filter { q } else { "" }]
        .iter()
        .any(|q| search::parse(q).fields.iter().any(|(name, _)| name == "text"));
    if uses_text {
        index_text(lib, &mut searcher);
    }
    let mut visible = browse::visible(&mut searcher, lib, &filter, &query);
    browse::sort(lib, &mut visible, sort);
    let found = visible.iter().map(|&i| &lib.refs[i]);
    if a.flag("json") {
        return print_json(&Value::Array(found.map(ref_json).collect()));
    }
    for r in found {
        out!("{}\t{}\t{}\t{}", r.key, r.year.map(|y| y.to_string()).unwrap_or_default(), short_authors(&r.authors), r.title);
    }
    Ok(())
}

/// Gives `searcher` the text of every PDF on this device, for `text:` terms.
fn index_text(lib: &Library, searcher: &mut Searcher) {
    let pdfs: Vec<(String, PathBuf)> =
        lib.refs.iter().flat_map(|r| r.pdfs.iter().filter_map(|p| Some((r.key.clone(), lib.pdf_path(p).ok()?)))).filter(|(_, p)| p.is_file()).collect();
    eprintln!("reading the text of {} PDFs…", pdfs.len());
    for (key, text) in pdftext::spawn(pdfs, lib.cache.clone()) {
        match text {
            Ok(t) => searcher.add_text(&key, &t),
            Err(e) => eprintln!("{key}: {e}"),
        }
    }
}

fn show_annotations(lib: &Library, dir: &Path, a: &Args) -> Result<()> {
    let [key] = &a.pos[..] else { bail!("name one reference by its key") };
    let r = &lib.refs[ops::ref_index(lib, key)?];
    let mut out = Vec::new();
    for pdf in &r.pdfs {
        let file = dir.join(annotations::file_path(key, pdf));
        let folder = dir.join(library::pdf_annotations_dir(key, pdf));
        for an in ops::load_annotations(dir, key, pdf) {
            let image = an.image.as_ref().map(|i| folder.join(i));
            if a.flag("json") {
                let mut v = serde_json::to_value(&an)?;
                v["pdf"] = pdf.id.clone().into();
                v["file"] = file.to_string_lossy().into();
                v["image"] = image.map(|p| p.to_string_lossy().into_owned()).into();
                out.push(v);
            } else {
                let what = [an.text.as_deref(), an.comment.as_deref(), image.as_deref().and_then(Path::to_str)];
                let what: Vec<&str> = what.into_iter().flatten().map(str::trim).filter(|s| !s.is_empty()).collect();
                out!("p. {}\t{}\t{}", an.page, an.kind, what.join(" · ").replace('\n', " "));
            }
        }
    }
    if a.flag("json") {
        print_json(&Value::Array(out))?;
    }
    Ok(())
}

fn add(lib: &mut Library, dir: &Path, a: &Args) -> Result<()> {
    anyhow::ensure!(!a.pos.is_empty(), "give a DOI, arXiv ID or ISBN");
    let ids: Vec<String> = a.pos.iter().flat_map(|p| yeda_core::ids::split_ids(p)).map(String::from).collect();
    let mut failed = 0;
    for id in ids {
        let result = (|| -> Result<(String, bool)> {
            let mut r = yeda_net::wand::fetch(&id)?;
            if let Some(x) = r.doi.as_deref().and_then(|d| lib.find_doi(d)) {
                return Ok((x.key.clone(), true));
            }
            r.key = lib.unique_key(&r);
            r.collections = a.opt("to").map(String::from).into_iter().collect();
            yeda_fs::apply(dir, [lib.save(&r)?])?;
            let key = r.key.clone();
            lib.refs.push(r);
            Ok((key, false))
        })();
        match result {
            Ok((key, false)) => out!("{key}"),
            Ok((key, true)) => {
                out!("{key}");
                eprintln!("{id}: already in the library as {key}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("{id}: {e:#}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} could not be added");
    Ok(())
}

fn new(lib: &Library, dir: &Path, a: &Args) -> Result<()> {
    let title = a.opt("title").filter(|t| !t.trim().is_empty()).context("give it a --title")?;
    let mut r = Ref {
        kind: a.opt("type").map_or("article".into(), str::to_lowercase),
        title: title.trim().into(),
        authors: a.opt("authors").unwrap_or_default().split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
        year: a.opt("year").map(|y| y.parse().context("the year is a number")).transpose()?,
        doi: a.opt("doi").map(str::to_lowercase),
        collections: a.opt("to").map(String::from).into_iter().collect(),
        ..Default::default()
    };
    if let Some(x) = r.doi.as_deref().and_then(|d| lib.find_doi(d)) {
        bail!("{} has this DOI already", x.key);
    }
    for k in crate::setup::EXTRAS {
        if let Some(v) = a.opt(k) {
            r.extra.insert(k.into(), v.into());
        }
    }
    r.key = lib.unique_key(&r);
    yeda_fs::apply(dir, [lib.save(&r)?])?;
    out!("{}", r.key);
    Ok(())
}

/// The location named by `--to`, else `fallback`, else the only one there is; with its WebDAV password.
fn location(lib: &Library, a: &Args, fallback: Option<&String>) -> Result<(String, Option<String>)> {
    let names: Vec<&String> = lib.locations.keys().chain(lib.remotes.keys()).collect();
    let location = match (a.opt("to"), fallback, names.as_slice()) {
        (Some(l), ..) => l.to_string(),
        (None, Some(l), _) => l.clone(),
        (None, None, [only]) => only.to_string(),
        _ => bail!("name a location with --to: {}", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
    };
    anyhow::ensure!(names.contains(&&location), "no location named '{location}'");
    let password = lib.remotes.get(&location).map(crate::dav_password).transpose()?;
    Ok((location, password))
}

fn attach(lib: &Library, dir: &Path, a: &Args) -> Result<()> {
    let [key, file] = &a.pos[..] else { bail!("usage: yeda attach <key> <file.pdf> [--to <location>]") };
    let r = &lib.refs[ops::ref_index(lib, key)?];
    let (location, password) = location(lib, a, None)?;
    let r = ops::attach_file(lib, r, &ops::expand_home(file), &location, password.as_deref())?;
    yeda_fs::apply(dir, [lib.save(&r)?])?;
    let pdf = r.pdfs.last().unwrap();
    eprintln!("attached {}:{} as PDF {}", pdf.location, pdf.path, pdf.id);
    Ok(())
}

fn detach(lib: &Library, dir: &Path, a: &Args) -> Result<()> {
    let [key] = &a.pos[..] else { bail!("name one reference by its key") };
    let mut r = lib.refs[ops::ref_index(lib, key)?].clone();
    let pdf = match (a.opt("pdf"), r.pdfs.as_slice()) {
        (Some(id), pdfs) => pdfs.iter().find(|p| p.id == id).with_context(|| format!("{key} has no PDF {id}"))?,
        (None, []) => bail!("{key} has no PDF"),
        (None, [only]) => only,
        (None, pdfs) => bail!("{key} has several PDFs; pick one with --pdf: {}", pdfs.iter().map(|p| format!("{} ({}:{})", p.id, p.location, p.path)).collect::<Vec<_>>().join(", ")),
    }
    .clone();
    r.pdfs.retain(|p| p.id != pdf.id);
    yeda_fs::apply(dir, [lib.save(&r)?, yeda_core::Effect::remove(library::pdf_annotations_dir(key, &pdf))])?;
    eprintln!("detached {}:{}", pdf.location, pdf.path);
    Ok(())
}

fn fetch_pdfs(lib: &mut Library, dir: &Path, a: &Args) -> Result<()> {
    let keys = keys(lib, &a.pos)?;
    let (location, password) = location(lib, a, lib.fulltext.location.clone().as_ref())?;
    let refs: Vec<&Ref> = keys.iter().filter_map(|k| lib.get(k)).collect();
    let request = ops::fetch_request(lib, refs, !a.flag("no-window"), None);
    anyhow::ensure!(!request.items.is_empty(), "they already have PDFs");
    let mut failed = 0;
    for msg in fetch::spawn(request) {
        progress("");
        let (key, result) = match msg {
            fetch::Msg::Progress(s) => {
                progress(&s);
                continue;
            }
            fetch::Msg::Done => break,
            fetch::Msg::Failed { key, reason } => (key, Err(anyhow::anyhow!(reason))),
            fetch::Msg::Saved { key, file, verify } => {
                let result = (|| {
                    let i = ops::ref_index(lib, &key)?;
                    if verify {
                        ops::check_download(&lib.refs[i], &file)?;
                    }
                    let r = ops::attach_file(lib, &lib.refs[i], &file, &location, password.as_deref())?;
                    yeda_fs::apply(dir, [lib.save(&r)?])?;
                    lib.refs[i] = r;
                    let _ = std::fs::remove_file(&file);
                    Ok(())
                })();
                (key, result)
            }
        };
        match result {
            Ok(()) => out!("{key}"),
            Err(e) => {
                failed += 1;
                eprintln!("{key}: {e:#}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} PDFs not fetched");
    Ok(())
}

fn cache(lib: &Library, a: &Args) -> Result<()> {
    let refs: Vec<Ref> = if a.pos.is_empty() { lib.refs.clone() } else { refs(lib, &a.pos)? };
    let pdfs: Vec<_> = refs.iter().flat_map(|r| &r.pdfs).filter_map(|p| Some((lib.remote_pdf(p)?, lib.pdf_path(p).ok()?))).collect();
    let mut passwords: HashMap<String, String> = HashMap::new();
    let mut failed = 0;
    for (n, (remote, path)) in pdfs.iter().enumerate() {
        let account = yeda_secrets::webdav_account(&remote.dav.user, &remote.dav.url);
        if !passwords.contains_key(&account) {
            passwords.insert(account.clone(), crate::dav_password(remote.dav)?);
        }
        eprint!("\r{}/{}", n + 1, pdfs.len());
        match ops::sync_pdf(remote.dav, &passwords[&account], &remote.path, path) {
            Ok(()) => {}
            Err(e) if yeda_net::webdav::is_unauthorized(&e) => return Err(e),
            Err(e) => {
                failed += 1;
                eprintln!("\r{}: {e:#}", remote.path);
            }
        }
    }
    eprintln!("\r{} of {} WebDAV PDFs cached", pdfs.len() - failed, pdfs.len());
    anyhow::ensure!(failed == 0, "{failed} failed");
    Ok(())
}

fn graph(lib: &mut Library, dir: &Path, a: &Args) -> Result<()> {
    let refs = refs(lib, &a.pos)?;
    if a.flag("tags") || a.opt("column").is_some() {
        let clusters = ops::clusters(lib, refs.iter().collect(), a.opt("column"))?;
        let labels: Vec<Value> = clusters
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::Label)
            .map(|n| {
                let papers: Vec<&str> = n.links.iter().filter_map(|&i| match &clusters.nodes[i].kind {
                    Kind::Paper(key) => Some(key.as_str()),
                    Kind::Label => None,
                }).collect();
                json!({ "label": n.name, "papers": papers })
            })
            .collect();
        return print_json(&Value::Array(labels));
    }
    let request = |key: Option<String>| citations::Request {
        refs: lib.refs.clone(),
        seeds: refs.iter().map(|r| r.key.clone()).collect(),
        settings: lib.graph.clone(),
        cache: lib.cache.clone().unwrap_or_else(std::env::temp_dir),
        key,
        refresh: a.flag("refresh"),
        explored: Vec::new(),
        opened: Vec::new(),
    };
    let mut key = None;
    let fetched = loop {
        let mut result = None;
        for msg in citations::spawn(request(key.clone())) {
            match msg {
                citations::Msg::Progress(s) => progress(&s),
                citations::Msg::Done(r) => result = Some(r.map_err(anyhow::Error::msg)),
                citations::Msg::OverBudget if key.is_none() => {
                    key = yeda_secrets::get(yeda_secrets::OPENALEX)?;
                    if key.is_none() {
                        bail!("OpenAlex's keyless daily budget is used up; set an API key in yeda with :setup openalex");
                    }
                }
                citations::Msg::OverBudget => bail!("OpenAlex's daily budget for this key is used up; what was fetched is cached"),
            }
        }
        progress("");
        if let Some(r) = result {
            break r?;
        }
    };
    let unknown = fetched.unknown.clone();
    let (graph, effects) = ops::finish_graph(lib, fetched);
    yeda_fs::apply(dir, effects.into_iter().collect::<Result<Vec<_>>>()?)?;
    let nodes: Vec<Value> = graph
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id, "key": n.key, "seed": n.seed, "title": n.title, "year": n.year, "first_author": n.first_author,
                "doi": n.doi, "cited_by": n.cited_by, "cited_by_seeds": n.cited_by_seeds, "cites_seeds": n.cites_seeds,
            })
        })
        .collect();
    let edges: Vec<Value> = graph.edges.iter().map(|&(citing, cited)| json!({ "citing": graph.nodes[citing].id, "cited": graph.nodes[cited].id })).collect();
    print_json(&json!({ "nodes": nodes, "edges": edges, "not_in_openalex": unknown }))
}

/// The API key or OAuth token for LLM profile `name`, from the secret store.
fn llm_key(name: &str, profile: &yeda_core::llm::Profile) -> Result<Option<String>> {
    let key = yeda_secrets::get(&crate::setup::llm_account(name, profile))?;
    anyhow::ensure!(key.is_some() || !profile.needs_key(), "no key stored for LLM profile '{name}'; set it in yeda with :setup llm {name}");
    Ok(key)
}

fn discover(lib: &Library, a: &Args) -> Result<()> {
    let prompt = a.pos.join(" ");
    let like = a.all("like");
    for k in &like {
        ops::ref_index(lib, k)?;
    }
    anyhow::ensure!(!prompt.trim().is_empty() || !like.is_empty(), "describe what to look for, or name references with --like");
    let (name, profile) = lib.llm_profile(a.opt("llm"))?;
    let key = llm_key(&name, &profile)?;
    let from: Vec<&Ref> = like.iter().filter_map(|k| lib.get(k)).collect();
    eprintln!("asking {name} at {}…", profile.url());
    let (found, note) = discover::spawn(profile, key, &prompt, &from, None).recv()??;
    if let Some(note) = note {
        eprintln!("note: {note}");
    }
    if a.flag("json") {
        let found: Vec<Value> = found
            .iter()
            .map(|c| {
                json!({
                    "id": c.id(), "title": c.title(), "authors": c.authors(), "year": c.work.as_ref().and_then(|w| w.year).or(c.found.year),
                    "why": c.found.why, "in_openalex": c.work.is_some(), "in_library": c.in_library(lib).map(|r| &r.key),
                })
            })
            .collect();
        return print_json(&Value::Array(found));
    }
    for c in &found {
        let mark = if c.in_library(lib).is_some() { "in library" } else if c.work.is_some() { "" } else { "not in OpenAlex" };
        let year = c.work.as_ref().and_then(|w| w.year).or(c.found.year).map(|y| y.to_string()).unwrap_or_default();
        out!("{}\t{year}\t{}\t{}\t{mark}", c.id().unwrap_or_default(), c.authors(), c.title());
    }
    Ok(())
}

fn summarize(lib: &mut Library, dir: &Path, a: &Args) -> Result<()> {
    let keys = keys(lib, &a.pos)?;
    let table_name = ops::table_name(lib, a.opt("table"))?;
    let table = lib.tables[&table_name].clone();
    let (profile_name, profile) = lib.llm_profile(a.opt("llm"))?;
    let overwrite = a.flag("overwrite");
    let mut jobs = Vec::new();
    for key in &keys {
        let r = &lib.refs[ops::ref_index(lib, key)?];
        let pdf = ops::pdfs_by_annotations(dir, r).into_iter().next().map(|(p, _)| p);
        let annots = pdf.as_ref().map(|p| ops::load_annotations(dir, key, p)).unwrap_or_default();
        jobs.extend(tables::plan(lib, &table_name, &table, r, pdf.as_ref(), &annots, overwrite));
    }
    anyhow::ensure!(!jobs.is_empty(), "nothing to fill in '{table_name}' (--overwrite regenerates existing cells)");
    let key = llm_key(&profile_name, &profile)?;
    let (total, mut failed) = (jobs.len(), 0);
    for (n, job) in jobs.into_iter().enumerate() {
        let k = job.key.clone();
        eprintln!("summarizing {k} ({}/{total}) with {profile_name} at {}…", n + 1, profile.url());
        let result = ops::summarize(&profile, job, key.as_deref()).and_then(|(row, note)| {
            let i = ops::ref_index(lib, &k)?;
            tables::merge(&mut lib.refs[i], &table_name, row, overwrite);
            yeda_fs::apply(dir, [lib.save_ai(&lib.refs[i])?])?;
            Ok(note)
        });
        match result {
            Ok(note) => {
                out!("{k}");
                if let Some(note) = note {
                    eprintln!("{k}: {note}");
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("{k}: {e:#}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} of {total} not summarized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options_flags_and_sort() {
        let args: Vec<String> = ["a", "--to", "x", "--json", "--llm=c", "--", "--b"].map(String::from).to_vec();
        let a = Args::parse(&args, &["to", "llm"], &["json"]).unwrap();
        assert_eq!(a.pos, ["a", "--b"]);
        assert_eq!((a.opt("to"), a.opt("llm"), a.flag("json")), (Some("x"), Some("c"), true));
        assert!(Args::parse(&["--nope".into()], &[], &[]).is_err());
        assert!(Args::parse(&["--to".into()], &["to"], &[]).is_err());
        assert_eq!(parse_sort("-year").unwrap(), Sort { field: SortField::Year, descending: true });
        assert!(parse_sort("bogus").is_err());
    }
}
