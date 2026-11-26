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

/// Copies `file` to a local location, or uploads it to a remote one keeping a cached copy;
/// returns `r` pointing at it, not yet saved.
pub fn attach_file(lib: &Library, r: &Ref, file: &Path, location: &str, password: Option<&str>) -> Result<Ref> {
    let bytes = fs::read(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    let mut r = r.clone();
    let pdf = lib.attach(&r, location, &file.to_string_lossy())?;
    let path = lib.pdf_path(&pdf)?;
    yeda_fs::write_atomic(&path, &bytes)?;
    if let (Some(remote), Some(password)) = (lib.remote_pdf(&pdf), password) {
        yeda_net::remote::upload(remote.remote, password, &remote.path, &bytes)?;
        let version = yeda_net::remote::current_version(remote.remote, password, &remote.path)?;
        yeda_fs::set_cached_version(&path, version.as_deref())?;
    }
    r.pdfs.push(pdf);
    Ok(r)
}

/// Copies `pdf` to `target`, which is already in another location. `source_secret` and
/// `target_secret` are the sign-ins of the two locations, for the remote ones.
pub fn copy_pdf(lib: &Library, pdf: &Pdf, target: &Pdf, source_secret: Option<&str>, target_secret: Option<&str>) -> Result<()> {
    let bytes = match lib.remote_pdf(pdf) {
        Some(remote) => yeda_net::remote::download(remote.remote, source_secret.unwrap_or_default(), &remote.path)?.0,
        None => {
            let path = lib.pdf_path(pdf)?;
            fs::read(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?
        }
    };
    let path = lib.pdf_path(target)?;
    yeda_fs::write_atomic(&path, &bytes).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    if let Some(remote) = lib.remote_pdf(target) {
        let secret = target_secret.unwrap_or_default();
        yeda_net::remote::upload(remote.remote, secret, &remote.path, &bytes)?;
        let version = yeda_net::remote::current_version(remote.remote, secret, &remote.path)?;
        yeda_fs::set_cached_version(&path, version.as_deref())?;
    }
    Ok(())
}

/// Copies every PDF at `from` to `to`, keeping each PDF's id so its annotations stay with it, and
/// returns the references now pointing at the copies. The originals stay where they are.
pub fn migrate(lib: &Library, from: &str, to: &str, from_secret: Option<&str>, to_secret: Option<&str>) -> Result<(Vec<Ref>, usize)> {
    let mut updated = Vec::new();
    let mut moved = 0;
    for r in lib.refs.iter().filter(|r| r.pdfs.iter().any(|p| p.location == from)) {
        let mut r = r.clone();
        let todo: Vec<Pdf> = r.pdfs.iter().filter(|p| p.location == from).cloned().collect();
        for pdf in todo {
            // attach names the copy `<citekey>/<file>` and gives it a new id; the id has to stay.
            let target = Pdf { id: pdf.id.clone(), ..lib.attach(&r, to, &pdf.path)? };
            copy_pdf(lib, &pdf, &target, from_secret, to_secret)?;
            if let Some(slot) = r.pdfs.iter_mut().find(|p| p.id == pdf.id) {
                *slot = target;
            }
            moved += 1;
        }
        updated.push(r);
    }
    Ok((updated, moved))
}

/// Sets `[locations.<name>]` in this device's config to the local folder `path`, making the folder.
/// A new location is appended, so the file's comments stay; changing one rewrites the file.
pub fn save_device_location(config: &Path, name: &str, path: &str) -> Result<()> {
    let folder = expand_home(path);
    fs::create_dir_all(&folder).map_err(|e| anyhow::anyhow!("{}: {e}", folder.display()))?;
    crate::sync::write_new(&folder.join(".stignore"), "*.tmp\n")?;
    let text = fs::read_to_string(config).unwrap_or_default();
    let mut device: toml::Table = toml::from_str(&text).context("device config")?;
    let known = device.get("locations").and_then(|l| l.get(name)).is_some();
    let locations = yeda_core::library::config_section(&mut device, "locations")?;
    locations.insert(name.into(), yeda_core::library::local_location(path));
    let written = if known {
        toml::to_string(&device)?
    } else {
        let only = toml::Table::from_iter([("locations".to_string(), toml::Value::Table(toml::Table::from_iter([(name.to_string(), yeda_core::library::local_location(path))])))]);
        format!("{}{}{}", text, if text.is_empty() || text.ends_with("\n\n") { "" } else { "\n" }, toml::to_string(&only)?)
    };
    yeda_fs::write_atomic(config, written.as_bytes())?;
    Ok(())
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

/// Downloads file `path` of `remote` to `cached` unless the copy there is the remote's current one.
pub fn sync_pdf(remote: &library::Remote, secret: &str, path: &str, cached: &Path) -> Result<()> {
    yeda_fs::sync_cached(cached, |version| {
        yeda_net::remote::current_version(remote, secret, path).map(|v| v.is_some() && v.as_deref() == version)
    }, || yeda_net::remote::download(remote, secret, path))
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

/// Renews a stored Codex credential in `key` and saves it, when it is due or `force`d. Returns
/// whether it was renewed; false for other providers, an environment-supplied key or a credential
/// that is not due.
pub fn renew_codex(b: &yeda_core::llm::Bound, key: &mut Option<String>, force: bool) -> Result<bool> {
    let account = yeda_secrets::provider_account(&b.provider, b.url());
    let Some(current) = key.as_deref() else { return Ok(false) };
    if b.auth() != yeda_core::llm::Auth::Codex || yeda_secrets::from_env(&account, b.settings.alias.as_deref()).is_some() || !(force || crate::setup::codex_refresh_due(current)) {
        return Ok(false);
    }
    let renewed = yeda_net::llm::codex_refresh(current)?;
    yeda_secrets::set(&account, &renewed)?;
    *key = Some(renewed);
    Ok(true)
}

/// `summarize` that renews a due Codex credential first, and once more after a 401; a rotated
/// refresh token is saved before the request that uses it.
pub fn summarize_renewing(bound: &yeda_core::llm::Bound, job: tables::Job, key: &mut Option<String>) -> Result<(Row, Option<String>)> {
    renew_codex(bound, key, false)?;
    match summarize(bound, job.clone(), key.as_deref()) {
        Err(e) if yeda_net::llm::is_unauthorized(&e) && renew_codex(bound, key, true)? => summarize(bound, job, key.as_deref()),
        sent => sent,
    }
}

/// Sends one paper to the LLM; returns its row and the LLM's note, if any.
pub fn summarize(bound: &yeda_core::llm::Bound, job: tables::Job, key: Option<&str>) -> Result<(Row, Option<String>)> {
    let bytes = job.pdf.as_ref().and_then(|p| fs::read(p).ok());
    let path = job.pdf.clone().unwrap_or_default();
    let text = || poppler::pdf_text(&path);
    let paper = bytes.as_deref().map(|pdf| Paper { pdf, text: &text });
    let prompt = Prompt { system: job.system, text: job.text, paper, schema: job.schema };
    let reply = llm::complete(bound, &prompt, key)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_due_codex_credential_is_renewed() {
        use yeda_core::llm::{Auth, Bound, Provider};
        let bound = |auth, base_url: &str| Bound { profile: "p".into(), provider: "p".into(), model: "m".into(), settings: Provider { base_url: base_url.into(), auth, alias: None } };
        let far = "eyJhbGciOiJub25lIn0.eyJleHAiOjE5MDAwMDAwMDB9.x".to_string();
        let mut key = Some(far.clone());
        assert!(!renew_codex(&bound(Auth::Codex, "https://codex-test.example"), &mut key, false).unwrap(), "not due");
        assert!(!renew_codex(&bound(Auth::ApiKey, "https://x.test/v1"), &mut key, true).unwrap(), "not Codex");
        assert!(!renew_codex(&bound(Auth::Codex, "https://codex-test.example"), &mut None, true).unwrap(), "no credential");
        assert_eq!(key, Some(far));
        let err = renew_codex(&bound(Auth::Codex, "https://codex-test.example"), &mut key, true).unwrap_err().to_string();
        assert!(err.contains("expired") || err.contains("sign in"), "a bare token cannot be renewed: {err}");
    }

    #[test]
    fn migration_copies_keeps_ids_and_leaves_the_originals() {
        let dir = std::env::temp_dir().join(format!("yeda-migrate-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        fs::create_dir_all(a.join("smith2020")).unwrap();
        fs::write(a.join("smith2020/paper.pdf"), b"%PDF-a").unwrap();
        let shared = format!(
            "[locations.a]\nkind = \"local\"\npath = \"{}\"\n\n[locations.b]\nkind = \"local\"\npath = \"{}\"\n",
            a.display(),
            b.display()
        );
        let refs = [("smith2020".to_string(), "title = \"A\"\n[[pdf]]\nid = \"1\"\nlocation = \"a\"\npath = \"smith2020/paper.pdf\"\n".to_string())];
        let lib = Library::parse(Some(&shared), None, refs, None).unwrap();
        let (updated, moved) = migrate(&lib, "a", "b", None, None).unwrap();
        assert_eq!(moved, 1);
        let pdf = &updated[0].pdfs[0];
        assert_eq!((pdf.id.as_str(), pdf.location.as_str(), pdf.path.as_str()), ("1", "b", "smith2020/paper.pdf"));
        assert_eq!(fs::read(b.join("smith2020/paper.pdf")).unwrap(), b"%PDF-a");
        assert!(a.join("smith2020/paper.pdf").is_file());
        // A missing file fails the whole run, before any reference changes.
        fs::remove_file(a.join("smith2020/paper.pdf")).unwrap();
        assert!(migrate(&lib, "a", "b", None, None).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_device_location_keeps_the_comments_until_it_changes() {
        let dir = std::env::temp_dir().join(format!("yeda-device-{}", std::process::id()));
        let config = dir.join("config.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&config, "# Settings for this device.\nlibrary = \"~/l\"\n").unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        save_device_location(&config, "papers", a.to_str().unwrap()).unwrap();
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.starts_with("# Settings for this device.\nlibrary = \"~/l\"\n"), "{text}");
        assert!(a.join(".stignore").is_file());
        let lib = Library::parse(None, Some(&text), [], None).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(lib.locations["papers"], a);

        save_device_location(&config, "papers", b.to_str().unwrap()).unwrap();
        save_device_location(&config, "more", a.to_str().unwrap()).unwrap();
        let lib = Library::parse(None, Some(&fs::read_to_string(&config).unwrap()), [], None).unwrap();
        assert_eq!((lib.locations["papers"].clone(), lib.locations["more"].clone()), (b, a));
        fs::remove_dir_all(&dir).unwrap();
    }
}
