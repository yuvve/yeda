//! Library operations shared by the TUI and the CLI subcommands.

use crate::{citations, fetch, poppler};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use yeda_core::Effect;
use yeda_core::annotations::{self, Annotation};
use yeda_core::clusters::Clusters;
use yeda_core::graph::{self, Graph};
use yeda_core::library::{self, Library, Pdf, Ref};
use yeda_core::tables::{self, Row};
use yeda_core::{cite, fulltext};
use yeda_net::llm::{self, Paper, Prompt};

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// `arg` if it names a table, else the only table there is.
pub fn table_name(lib: &Library, arg: Option<&str>) -> Result<String> {
    let names = || lib.tables.keys().map(String::as_str).collect::<Vec<_>>().join(", ");
    match arg {
        Some(n) if lib.tables.contains_key(n) => Ok(n.to_string()),
        Some(n) => anyhow::bail!("no table named '{n}' (tables: {})", names()),
        None if lib.tables.len() == 1 => Ok(lib.tables.keys().next().unwrap().clone()),
        None if lib.tables.is_empty() => anyhow::bail!("no tables defined: add [tables.<name>] to yeda.toml or run import-table"),
        None => anyhow::bail!("several tables; name one: {}", names()),
    }
}

pub fn load_annotations(dir: &Path, key: &str, pdf: &Pdf) -> Vec<Annotation> {
    let text = yeda_fs::read_optional(&dir.join(annotations::file_path(key, pdf))).ok().flatten();
    text.and_then(|t| annotations::parse(&t).ok()).unwrap_or_default()
}

/// `r`'s PDFs, the most annotated first.
pub fn pdfs_by_annotations(dir: &Path, r: &Ref) -> Vec<(Pdf, usize)> {
    let mut pdfs: Vec<(Pdf, usize)> = r.pdfs.iter().map(|p| (p.clone(), load_annotations(dir, &r.key, p).len())).collect();
    pdfs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    pdfs
}

/// `refs` as `format`, else `[cite] format`, else BibTeX; a `.csl` path is relative to the library.
pub fn cite_text(lib: &Library, dir: &Path, refs: &[Ref], format: &str) -> Result<(String, String)> {
    let format = if format.is_empty() { lib.cite.format.clone().unwrap_or_else(|| "bibtex".into()) } else { format.to_string() };
    let read = |path: &str| {
        let path = if path.starts_with("~/") { expand_home(path) } else { dir.join(path) };
        fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
    };
    Ok((cite::export(refs, &format, read)?, format))
}

/// Copies `file` to a local location, or uploads it to a WebDAV one keeping a cached copy;
/// returns `r` pointing at it, not yet saved.
pub fn attach_file(lib: &Library, r: &Ref, file: &Path, location: &str, password: Option<&str>) -> Result<Ref> {
    let bytes = fs::read(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    let mut r = r.clone();
    let pdf = lib.attach(&r, location, &file.to_string_lossy())?;
    let path = lib.pdf_path(&pdf)?;
    yeda_fs::write_atomic(&path, &bytes)?;
    if let (Some(remote), Some(password)) = (lib.remote_pdf(&pdf), password) {
        yeda_net::webdav::upload(remote.dav, password, &remote.path, &bytes)?;
        let version = yeda_net::webdav::current_version(remote.dav, password, &remote.path)?;
        yeda_fs::set_cached_version(&path, version.as_deref())?;
    }
    r.pdfs.push(pdf);
    Ok(r)
}

/// Refuses a PDF from the fetch window that doesn't look like `r`, since any link could have been clicked there.
pub fn check_download(r: &Ref, file: &Path) -> Result<()> {
    let text = poppler::pdf_text(file).unwrap_or_default();
    anyhow::ensure!(fulltext::looks_like(&text, r), "the downloaded PDF doesn't look like this paper (no DOI or title match); it's in {}", file.display());
    if let (Some(max), Ok(pages)) = (fulltext::max_pages(r), poppler::page_count(file)) {
        anyhow::ensure!(pages <= max, "the downloaded PDF has {pages} pages, more than this paper should (a whole issue?); if it's right, :attach {}", file.display());
    }
    Ok(())
}

/// What `:fetch` looks for: the references among `refs` without a PDF.
pub fn fetch_request<'a>(lib: &Library, refs: impl IntoIterator<Item = &'a Ref>, window: bool, openalex_key: Option<String>) -> fetch::Request {
    let items = refs
        .into_iter()
        .filter(|r| r.pdfs.is_empty())
        .map(|r| fetch::Item { key: r.key.clone(), doi: graph::lookup_doi(r), openalex: graph::work_id(r).map(String::from), file: fulltext::file_name(r) })
        .collect();
    let downloads = match &lib.fulltext.downloads {
        Some(d) => expand_home(&d.to_string_lossy()),
        None => dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("yeda/downloads"),
    };
    fetch::Request { items, settings: lib.fulltext.clone(), downloads, openalex_key, window }
}

/// Downloads WebDAV file `remote` to `cached` unless the copy there is the server's current one.
pub fn sync_pdf(dav: &library::Dav, password: &str, remote: &str, cached: &Path) -> Result<()> {
    yeda_fs::sync_cached(cached, |version| {
        yeda_net::webdav::current_version(dav, password, remote).map(|v| v.is_some() && v.as_deref() == version)
    }, || yeda_net::webdav::download(dav, password, remote))
}

/// Extracts the annotations embedded in each of `r`'s PDFs, handing what to write to `write`.
pub fn extract_annotations(lib: &Library, dir: &Path, r: &Ref, write: &mut dyn FnMut(Vec<Effect>) -> Result<()>) -> Result<usize> {
    anyhow::ensure!(!r.pdfs.is_empty(), "no pdf");
    let mut total = 0;
    for p in &r.pdfs {
        let pdf = lib.pdf_path(p)?;
        let bytes = fs::read(&pdf).map_err(|e| anyhow::anyhow!("{}: {e}", pdf.display()))?;
        let words = poppler::word_boxes(&pdf)?;
        let rel = library::pdf_annotations_dir(&r.key, p);
        total += yeda_fs::locked(&dir.join(&rel), || {
            let existing = yeda_fs::read_optional(&dir.join(annotations::file_path(&r.key, p)))?;
            let mut render = |page, height, rect| poppler::render_region(&pdf, page, height, rect).ok();
            let (n, effects) = annotations::sync_from_pdf(&rel, existing.as_deref(), &bytes, Some(&words), &mut render)?;
            write(effects)?;
            Ok(n)
        })?;
    }
    Ok(total)
}

/// Sends one paper to the LLM; returns its row and the LLM's note, if any.
pub fn summarize(profile: &yeda_core::llm::Profile, job: tables::Job, key: Option<&str>) -> Result<(Row, Option<String>)> {
    let bytes = job.pdf.as_ref().and_then(|p| fs::read(p).ok());
    let path = job.pdf.clone().unwrap_or_default();
    let text = || poppler::pdf_text(&path);
    let paper = bytes.as_deref().map(|pdf| Paper { pdf, text: &text });
    let prompt = Prompt { system: job.system, text: job.text, paper, schema: job.schema };
    let reply = llm::complete(profile, &prompt, key)?;
    Ok((tables::parse_row(&reply.text, &job.columns)?, reply.note))
}

/// `refs` and their tags, or their values in table column `column`.
pub fn clusters(lib: &Library, refs: Vec<&Ref>, column: Option<&str>) -> Result<Clusters> {
    let clusters = match column {
        None => Clusters::build(refs, |r| r.tags.clone()),
        Some(column) => {
            let Some(table) = lib.tables.iter().find(|(_, t)| t.columns.iter().any(|c| c == column)).map(|(n, _)| n.clone()) else {
                let all: Vec<&str> = lib.tables.values().flat_map(|t| t.columns.iter().map(String::as_str)).collect();
                anyhow::bail!("no table has a column '{column}' (columns: {})", all.join(", "));
            };
            Clusters::build(refs, |r| r.tables.get(&table).and_then(|row| row.get(column)).cloned().into_iter().collect())
        }
    };
    anyhow::ensure!(!clusters.nodes.is_empty(), "none of these references has {}", column.map_or("tags".to_string(), |c| format!("a value for '{c}'")));
    Ok(clusters)
}

/// The graph a fetch found, with the opened papers' lists and the co-cited works added; records
/// newly matched OpenAlex ids in `lib.refs`, returning the saves that keep them.
pub fn finish_graph(lib: &mut Library, f: citations::Fetched) -> (Graph, Vec<Result<Effect>>) {
    let mut effects = Vec::new();
    for (key, id) in &f.matched {
        if let Some(i) = lib.refs.iter().position(|r| &r.key == key) {
            graph::set_work_id(&mut lib.refs[i], id);
            effects.push(lib.save(&lib.refs[i]));
        }
    }
    let mut graph = f.graph;
    let keys = lib.refs.iter().filter_map(|r| Some((graph::work_id(r)?.to_string(), r.key.clone()))).collect();
    for (id, lists) in &f.lists {
        if let Some(i) = graph.find(id) {
            graph.extend(i, &lists.cites, &lists.cited_by, &keys);
        }
    }
    for (id, list) in &f.cocited {
        if let Some(i) = graph.find(id) {
            graph.add_cocited(i, list, &keys);
        }
    }
    graph.relayout();
    (graph, effects)
}

pub fn ref_index(lib: &Library, key: &str) -> Result<usize> {
    lib.refs.iter().position(|r| r.key == key).with_context(|| format!("no reference with key '{key}'"))
}
