//! yeda-android's Tauri commands: the library browser, next to yeda-reader's commands for the PDF view.

mod git;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};
use yeda_core::annotations::{self, Annotation};
use yeda_core::browse::{self, Filter, Sort, SortField};
use yeda_core::library::{self, Library, Ref, config_section};
use yeda_core::search::{self, FILTER_FIELDS, Searcher};
use yeda_core::{Effect, fulltext, graph, tables};
use yeda_net::llm::{self, Paper, Prompt};
use yeda_net::wand;
use yeda_reader_lib::{Current, Doc};

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Per-device settings in the app's data folder (`settings.toml`): the library folder, and
/// everything else the library's machine config takes, e.g. `[locations]` overriding `yeda.toml`
/// for folders that live elsewhere on this device. Secrets go in the Keystore, never here.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    library: Option<PathBuf>,
    #[serde(flatten)]
    machine: toml::Table,
}

fn settings_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("settings.toml"))
}

fn load_settings(app: &AppHandle) -> anyhow::Result<Settings> {
    let text = yeda_fs::read_optional(&settings_path(app)?)?;
    Ok(text.map(|t| toml::from_str(&t)).transpose()?.unwrap_or_default())
}

fn update_settings(app: &AppHandle, edit: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
    let mut s = load_settings(app)?;
    edit(&mut s);
    Ok(yeda_fs::write_atomic(&settings_path(app)?, toml::to_string(&s)?.as_bytes())?)
}

struct Browser {
    dir: Option<PathBuf>,
    lib: Library,
    searcher: Searcher,
}

#[derive(Default)]
struct Shared(Mutex<Option<Browser>>);

impl Browser {
    fn load(app: &AppHandle) -> anyhow::Result<Browser> {
        let dir = load_settings(app)?.library;
        let mut lib = match &dir {
            Some(d) => yeda_fs::load_library(d, Some(&settings_path(app)?), None)?,
            None => Library::default(),
        };
        lib.cache = Some(app.path().app_cache_dir()?.join("webdav"));
        Ok(Browser { dir, lib, searcher: Searcher::new() })
    }

    fn dir(&self) -> anyhow::Result<&Path> {
        self.dir.as_deref().context("no library folder chosen")
    }

    fn path(&self, rel: &str) -> anyhow::Result<PathBuf> {
        Ok(self.dir()?.join(rel))
    }

    fn apply(&self, effects: Vec<Effect>) -> anyhow::Result<()> {
        yeda_fs::apply(self.dir()?, effects)
    }

    fn get(&self, key: &str) -> anyhow::Result<&Ref> {
        self.lib.get(key).with_context(|| format!("no reference {key}"))
    }

    fn edit(&mut self, key: &str, change: impl FnOnce(&mut Ref)) -> anyhow::Result<()> {
        let i = self.lib.refs.iter().position(|r| r.key == key).with_context(|| format!("no reference {key}"))?;
        change(&mut self.lib.refs[i]);
        self.apply(vec![self.lib.save(&self.lib.refs[i])?])
    }

    fn pdf(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let pdf = self.lib.pdf_path(self.get(key)?)?;
        std::fs::read(&pdf).with_context(|| pdf.display().to_string())
    }

    /// Runs `sync_from_pdf` for `key` with `words` (`pdftotext -bbox` format) and `render_png`.
    fn sync_from_pdf(&self, key: &str, words: Option<&str>, render_png: &mut annotations::RenderPng) -> anyhow::Result<(usize, Vec<Effect>)> {
        let existing = yeda_fs::read_optional(&self.path(&annotations::file_path(key))?)?;
        annotations::sync_from_pdf(&library::annotations_dir(key), existing.as_deref(), &self.pdf(key)?, words, render_png)
    }

    fn annotations(&self, key: &str) -> anyhow::Result<Vec<Annotation>> {
        let text = yeda_fs::read_optional(&self.path(&annotations::file_path(key))?)?;
        Ok(text.as_deref().map(annotations::parse).transpose()?.unwrap_or_default())
    }
}

impl Shared {
    /// Runs `f` on the library, loading it first if needed.
    fn with<T>(&self, app: &AppHandle, f: impl FnOnce(&mut Browser) -> anyhow::Result<T>) -> Res<T> {
        let mut guard = self.0.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Browser::load(app).map_err(err)?);
        }
        f(guard.as_mut().unwrap()).map_err(err)
    }

    /// Like `with`, then re-reads the library so config edits take effect.
    fn change<T>(&self, app: &AppHandle, f: impl FnOnce(&mut Browser) -> anyhow::Result<T>) -> Res<T> {
        let out = self.with(app, f)?;
        *self.0.lock().unwrap() = Some(Browser::load(app).map_err(err)?);
        Ok(out)
    }
}

#[tauri::command]
fn library(app: AppHandle) -> Res<Option<PathBuf>> {
    load_settings(&app).map(|s| s.library).map_err(err)
}

#[tauri::command]
fn set_library(app: AppHandle, state: State<Shared>, path: PathBuf) -> Res<()> {
    update_settings(&app, |s| s.library = Some(path)).map_err(err)?;
    state.change(&app, |_| Ok(()))
}

#[tauri::command]
fn set_location(app: AppHandle, state: State<Shared>, name: String, path: PathBuf) -> Res<()> {
    let path = path.to_string_lossy().into_owned();
    let mut edit = Ok(());
    update_settings(&app, |s| edit = config_section(&mut s.machine, "locations").map(|l| drop(l.insert(name, path.into()))))
        .map_err(err)?;
    edit.map_err(err)?;
    state.change(&app, |_| Ok(()))
}

/// `settings.toml` as text, for editing.
#[tauri::command]
fn settings_text(app: AppHandle) -> Res<String> {
    Ok(yeda_fs::read_optional(&settings_path(&app).map_err(err)?).map_err(err)?.unwrap_or_default())
}

/// Replaces `settings.toml`, once it parses both as settings and as a machine config.
#[tauri::command]
fn save_settings_text(app: AppHandle, state: State<Shared>, text: String) -> Res<()> {
    toml::from_str::<Settings>(&text).map_err(|e| e.to_string())?;
    Library::parse(None, Some(&text), [], None).map_err(err)?;
    yeda_fs::write_atomic(&settings_path(&app).map_err(err)?, text.as_bytes()).map_err(|e| e.to_string())?;
    state.change(&app, |_| Ok(()))
}

#[derive(Serialize)]
struct KeyStatus {
    env: String,
    set: bool,
}

/// The API key each LLM profile needs, and whether this device has it.
#[tauri::command]
fn llm_keys(app: AppHandle, state: State<Shared>) -> Res<Vec<KeyStatus>> {
    state.with(&app, |b| {
        let mut envs: Vec<String> = b.lib.llm.values().map(|p| p.key_env().to_string()).collect();
        envs.dedup();
        envs.into_iter()
            .map(|env| Ok(KeyStatus { set: yeda_secrets::get(&yeda_secrets::llm_account(&env))?.is_some(), env }))
            .collect()
    })
}

/// Saves an LLM API key in the Keystore; an empty key removes it.
#[tauri::command]
fn set_key(env: String, key: String) -> Res<()> {
    let account = yeda_secrets::llm_account(&env);
    match key.trim() {
        "" => yeda_secrets::delete(&account),
        k => yeda_secrets::set(&account, k),
    }
    .map_err(err)
}

/// Saves a WebDAV password in the Keystore.
#[tauri::command]
fn set_secret(account: String, secret: String) -> Res<()> {
    yeda_secrets::set(&account, &secret).map_err(err)
}

#[derive(Serialize)]
struct SideEntry {
    filter: Filter,
    color: Option<String>,
}

#[derive(Serialize)]
struct Overview {
    sidebar: Vec<SideEntry>,
    statuses: &'static [&'static str],
    tables: Vec<String>,
    /// `(field, label)` for the filter form.
    filter_fields: &'static [(&'static str, &'static str)],
    /// `(field, label)` for sorting.
    sort_fields: Vec<(SortField, &'static str)>,
    /// Every location a PDF can be attached to, local and WebDAV.
    locations: Vec<String>,
    /// The library's git remote, if it is in a git repository.
    git: Option<String>,
}

/// Re-reads the library from disk, since Syncthing or another client may have changed it.
#[tauri::command]
fn refresh(app: AppHandle, state: State<Shared>) -> Res<Overview> {
    state.change(&app, |_| Ok(()))?;
    state.with(&app, |b| {
        let sidebar = browse::sidebar(&b.lib)
            .into_iter()
            .map(|filter| {
                let color = match &filter {
                    Filter::Tag(t) => b.lib.tag_colors.get(t).cloned(),
                    _ => None,
                };
                SideEntry { filter, color }
            })
            .collect();
        Ok(Overview {
            sidebar,
            statuses: &library::STATUSES,
            tables: b.lib.tables.keys().cloned().collect(),
            filter_fields: &FILTER_FIELDS,
            sort_fields: SortField::ALL.iter().map(|f| (*f, f.name())).collect(),
            locations: b.lib.locations.keys().chain(b.lib.remotes.keys()).cloned().collect(),
            git: b.dir().ok().and_then(git::remote_url),
        })
    })
}

#[derive(Serialize)]
struct Row {
    key: String,
    title: String,
    authors: Vec<String>,
    year: Option<i32>,
    status: Option<String>,
    tags: Vec<String>,
    has_pdf: bool,
}

#[tauri::command]
fn list(app: AppHandle, state: State<Shared>, filter: Filter, query: String, sort: Sort) -> Res<Vec<Row>> {
    state.with(&app, |b| {
        let mut visible = browse::visible(&mut b.searcher, &b.lib, &filter, &query);
        browse::sort(&b.lib, &mut visible, sort);
        Ok(visible
            .into_iter()
            .map(|i| {
                let r = &b.lib.refs[i];
                Row {
                    key: r.key.clone(),
                    title: r.title.clone(),
                    authors: r.authors.clone(),
                    year: r.year,
                    status: r.status.clone(),
                    tags: r.tags.clone(),
                    has_pdf: r.pdf.is_some(),
                }
            })
            .collect())
    })
}

#[derive(Serialize)]
struct QueryFields {
    fields: Vec<(String, String)>,
    free: String,
}

/// A query's field terms and free text, for the filter form.
#[tauri::command]
fn parse_query(query: String) -> QueryFields {
    let q = search::parse(&query);
    QueryFields { fields: q.fields, free: q.free.replace("\\ ", " ") }
}

/// Existing values of each filter form field.
#[tauri::command]
fn filter_suggestions(app: AppHandle, state: State<Shared>) -> Res<std::collections::HashMap<&'static str, Vec<String>>> {
    state.with(&app, |b| Ok(FILTER_FIELDS.iter().map(|(f, _)| (*f, search::suggestions(&b.lib.refs, f))).collect()))
}

#[tauri::command]
fn compose_query(fields: Vec<(String, String)>, free: String) -> String {
    search::compose(&fields, &free)
}

#[derive(Serialize)]
struct Details {
    key: String,
    #[serde(rename = "ref")]
    reference: Ref,
    raw: String,
    note: String,
    annotations: Vec<Annotation>,
    /// Each table's columns, for showing this reference's rows in order.
    tables: BTreeMap<String, Vec<String>>,
}

#[tauri::command]
fn details(app: AppHandle, state: State<Shared>, key: String) -> Res<Details> {
    state.with(&app, |b| {
        let reference = b.get(&key)?.clone();
        let raw = yeda_fs::read_optional(&b.path(&library::ref_path(&key))?)?.unwrap_or_default();
        let note = yeda_fs::read_optional(&b.path(&library::note_path(&key))?)?.unwrap_or_default();
        let tables = b.lib.tables.iter().map(|(n, t)| (n.clone(), t.columns.clone())).collect();
        Ok(Details { annotations: b.annotations(&key)?, key, reference, raw, note, tables })
    })
}

#[tauri::command]
fn annotation_image(app: AppHandle, state: State<Shared>, key: String, image: String) -> Res<Response> {
    state.with(&app, |b| {
        let dir = b.path(&library::annotations_dir(&key))?;
        let path = dir.join(&image);
        anyhow::ensure!(path.starts_with(&dir) && !image.contains(".."), "invalid image path {image}");
        Ok(Response::new(std::fs::read(&path).with_context(|| path.display().to_string())?))
    })
}

#[tauri::command]
fn set_status(app: AppHandle, state: State<Shared>, key: String, status: Option<String>) -> Res<()> {
    state.with(&app, |b| b.edit(&key, |r| library::set_status(r, status)))
}

#[tauri::command]
fn set_tags(app: AppHandle, state: State<Shared>, key: String, tags: Vec<String>) -> Res<()> {
    state.with(&app, |b| b.edit(&key, |r| r.tags = tags))
}

#[tauri::command]
fn set_collection(app: AppHandle, state: State<Shared>, key: String, collection: Option<String>) -> Res<()> {
    state.with(&app, |b| b.edit(&key, |r| r.collections = collection.into_iter().collect()))
}

#[tauri::command]
fn set_tag_color(app: AppHandle, state: State<Shared>, tag: String, color: String) -> Res<()> {
    state.change(&app, |b| {
        let effect = b.lib.update_config(|t| {
            let colors = config_section(t, "tag_colors")?;
            match color.trim() {
                "" => drop(colors.remove(&tag)),
                c => drop(colors.insert(tag, c.into())),
            }
            Ok(())
        })?;
        b.apply(vec![effect])
    })
}

#[tauri::command]
fn save_search(app: AppHandle, state: State<Shared>, name: String, query: String) -> Res<()> {
    state.change(&app, |b| {
        let effect = b.lib.update_config(|t| {
            config_section(t, "searches")?.insert(name, query.into());
            Ok(())
        })?;
        b.apply(vec![effect])
    })
}

#[tauri::command]
fn delete_search(app: AppHandle, state: State<Shared>, name: String) -> Res<()> {
    state.change(&app, |b| {
        let effect = b.lib.update_config(|t| {
            config_section(t, "searches")?.remove(&name);
            Ok(())
        })?;
        b.apply(vec![effect])
    })
}

#[derive(Serialize)]
struct Added {
    key: String,
    existed: bool,
}

#[tauri::command]
fn split_ids(text: String) -> Vec<String> {
    yeda_core::ids::split_ids(&text).into_iter().map(Into::into).collect()
}

#[tauri::command]
async fn add_ref(app: AppHandle, state: State<'_, Shared>, input: String) -> Res<Added> {
    let fetched = tauri::async_runtime::spawn_blocking(move || wand::fetch(&input)).await.map_err(|e| e.to_string())?;
    let mut r = fetched.map_err(err)?;
    state.change(&app, |b| {
        if let Some(x) = r.doi.as_deref().and_then(|d| b.lib.find_doi(d)) {
            return Ok(Added { key: x.key.clone(), existed: true });
        }
        r.key = b.lib.unique_key(&r);
        b.apply(vec![b.lib.save(&r)?])?;
        Ok(Added { key: r.key, existed: false })
    })
}

#[tauri::command]
fn delete_ref(app: AppHandle, state: State<Shared>, key: String) -> Res<()> {
    state.change(&app, |b| b.apply(b.lib.delete(&key)))
}

/// Writes the reference file as typed, once it parses.
#[tauri::command]
fn save_raw(app: AppHandle, state: State<Shared>, key: String, text: String) -> Res<()> {
    state.change(&app, |b| {
        library::parse_ref(key.clone(), &text)?;
        b.apply(vec![Effect::write(library::ref_path(&key), text)])
    })
}

#[tauri::command]
fn save_note(app: AppHandle, state: State<Shared>, key: String, text: String) -> Res<()> {
    state.with(&app, |b| {
        let path = library::note_path(&key);
        b.apply(vec![if text.trim().is_empty() { Effect::remove(path) } else { Effect::write(path, text) }])
    })
}

/// The PDF of reference `key`, for pdf.js in the WebView, which stands in for poppler on Android.
#[tauri::command]
fn ref_pdf(app: AppHandle, state: State<Shared>, key: String) -> Res<Response> {
    state.with(&app, |b| Ok(Response::new(b.pdf(&key)?)))
}

#[derive(Serialize)]
struct Region {
    /// 1-based.
    page: u32,
    height: f32,
    /// PDF coordinates: `[x0, y0, x1, y1]`, origin bottom-left.
    rect: [f32; 4],
}

/// The regions `extract_annotations` needs rendered, in the order it asks for them.
#[tauri::command]
fn pdf_image_regions(app: AppHandle, state: State<Shared>, key: String) -> Res<Vec<Region>> {
    state.with(&app, |b| {
        let mut regions = Vec::new();
        b.sync_from_pdf(&key, None, &mut |page, height, rect| {
            regions.push(Region { page, height, rect });
            None
        })?;
        Ok(regions)
    })
}

/// `words` and `images` come from pdf.js: the page words in `pdftotext -bbox` format, and a PNG
/// (or null) for each of `pdf_image_regions` in order.
#[tauri::command]
fn extract_annotations(app: AppHandle, state: State<Shared>, key: String, words: Option<String>, images: Vec<Option<Vec<u8>>>) -> Res<usize> {
    state.with(&app, |b| {
        let mut images = images.into_iter();
        let (n, effects) = b.sync_from_pdf(&key, words.as_deref(), &mut |_, _, _| images.next().flatten())?;
        b.apply(effects)?;
        Ok(n)
    })
}

/// Fills `key`'s row of `table` with the default LLM profile; `text` is the paper's text from pdf.js,
/// for providers that take text rather than the PDF. Returns a note on how it went.
#[tauri::command]
async fn summarize(app: AppHandle, state: State<'_, Shared>, key: String, table: String, overwrite: bool, text: Option<String>) -> Res<String> {
    let (job, profile) = state.with(&app, |b| {
        let def = b.lib.tables.get(&table).with_context(|| format!("no table named '{table}'"))?;
        let r = b.get(&key)?;
        let job = tables::plan(&b.lib, &table, def, r, &b.annotations(&key)?, overwrite)
            .with_context(|| format!("nothing to fill in '{table}'"))?;
        Ok((job, b.lib.llm_profile(None)?.1))
    })?;
    let api_key = yeda_secrets::get(&yeda_secrets::llm_account(profile.key_env())).map_err(err)?;
    let reply = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<_> {
        let bytes = job.pdf.as_ref().and_then(|p| std::fs::read(p).ok());
        let text = || text.clone().context("the PDF's text could not be extracted");
        let paper = bytes.as_deref().map(|pdf| Paper { pdf, text: &text });
        let prompt = Prompt { system: job.system, text: job.text, paper, schema: job.schema };
        let reply = llm::complete(&profile, &prompt, api_key.as_deref())?;
        Ok((tables::parse_row(&reply.text, &job.columns)?, reply.note))
    })
    .await
    .map_err(|e| e.to_string())?;
    let (row, note) = reply.map_err(err)?;
    state.with(&app, |b| b.edit(&key, |r| tables::merge(r, &table, row, overwrite)))?;
    Ok(note.unwrap_or_else(|| format!("filled '{table}'")))
}

/// Writes the table as CSV to the device's Download folder; returns the file's path.
#[tauri::command]
fn export_table(app: AppHandle, state: State<Shared>, table: String) -> Res<String> {
    state.with(&app, |b| {
        let (csv, _) = tables::export_csv(&b.lib, &table)?;
        let path = app.path().download_dir()?.join(format!("{table}.csv"));
        yeda_fs::write_atomic(&path, csv.as_bytes()).with_context(|| path.display().to_string())?;
        Ok(path.display().to_string())
    })
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum OpenRef {
    Opened,
    MissingLocation { location: String },
    /// The password for `account` isn't in the Keystore, or the server rejected it.
    NeedsPassword { account: String, label: String, rejected: bool },
    Synced { message: String },
}

/// Runs a git operation off the main thread with the Keystore's token for `url`;
/// a missing or rejected token becomes `NeedsPassword`.
async fn with_git_login(url: String, run: impl FnOnce(&git::Login) -> anyhow::Result<String> + Send + 'static) -> Res<OpenRef> {
    let account = yeda_secrets::git_account(&url);
    let token = yeda_secrets::get(&account).map_err(err)?;
    let rejected = token.is_some();
    let (result, refused) = tauri::async_runtime::spawn_blocking(move || {
        let refused = std::cell::Cell::new(false);
        let result = run(&git::Login { token: token.as_deref(), refused: &refused });
        (result, refused.get())
    })
    .await
    .map_err(|e| e.to_string())?;
    match result {
        Ok(message) => Ok(OpenRef::Synced { message }),
        Err(_) if refused => Ok(OpenRef::NeedsPassword { account, label: url, rejected }),
        Err(e) => Err(err(e)),
    }
}

#[tauri::command]
async fn git_sync(app: AppHandle, state: State<'_, Shared>) -> Res<OpenRef> {
    let dir = state.with(&app, |b| Ok(b.dir()?.to_path_buf()))?;
    let url = git::remote_url(&dir).context("the library folder has no git remote named origin").map_err(err)?;
    let out = with_git_login(url, move |login| git::sync(&dir, login)).await?;
    state.change(&app, |_| Ok(()))?;
    Ok(out)
}

/// Where fetched PDFs wait to be attached.
fn downloads_dir(app: &AppHandle, settings: &fulltext::Settings) -> anyhow::Result<PathBuf> {
    match &settings.downloads {
        Some(dir) => Ok(dir.clone()),
        None => Ok(app.path().app_data_dir()?.join("downloads")),
    }
}

#[derive(Serialize)]
struct FetchPaper {
    key: String,
    doi: Option<String>,
    file: String,
}

/// What `FetchActivity` and the browser screen need for fetching PDFs.
#[derive(Serialize)]
struct FetchPlan {
    downloads: PathBuf,
    proxy: Option<String>,
    proxy_host: Option<String>,
    location: Option<String>,
    papers: Vec<FetchPaper>,
}

/// The plan for those of `keys` without a PDF.
#[tauri::command]
fn fetch_plan(app: AppHandle, state: State<Shared>, keys: Vec<String>) -> Res<FetchPlan> {
    state.with(&app, |b| {
        let settings = &b.lib.fulltext;
        let papers = keys
            .iter()
            .filter_map(|k| b.lib.get(k))
            .filter(|r| r.pdf.is_none())
            .map(|r| FetchPaper { key: r.key.clone(), doi: graph::lookup_doi(r), file: fulltext::file_name(r) })
            .collect();
        let downloads = downloads_dir(&app, settings)?;
        std::fs::create_dir_all(&downloads)?;
        Ok(FetchPlan {
            downloads,
            proxy: settings.proxy.clone(),
            proxy_host: fulltext::proxy_host(settings),
            location: settings.location.clone(),
            papers,
        })
    })
}

/// Downloads an open-access copy, else the publisher's PDF (which works on a campus network),
/// into `downloads`; `None` if neither is there and the paper needs the browser.
#[tauri::command]
async fn fetch_direct(app: AppHandle, state: State<'_, Shared>, doi: String, file: String) -> Res<Option<PathBuf>> {
    let settings = state.with(&app, |b| Ok(b.lib.fulltext.clone()))?;
    let target = downloads_dir(&app, &settings).map_err(err)?.join(&file);
    let key = yeda_secrets::get(yeda_secrets::OPENALEX).ok().flatten();
    tauri::async_runtime::spawn_blocking(move || {
        let client = yeda_net::openalex::Client::new(key);
        let urls = client.pdf_urls(&doi).unwrap_or_default().into_iter().chain(fulltext::publisher_urls(&doi, &settings));
        let Some(pdf) = urls.into_iter().find_map(|u| yeda_net::fulltext::download(&u, None).ok()) else { return Ok(None) };
        std::fs::write(&target, pdf).map_err(|e| format!("{}: {e}", target.display()))?;
        Ok(Some(target))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Only files in the downloads folder may be read or removed through these.
fn in_downloads(app: &AppHandle, state: &State<Shared>, path: &Path) -> Res<PathBuf> {
    let dir = state.with(app, |b| downloads_dir(app, &b.lib.fulltext))?;
    let path = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.starts_with(dir.canonicalize().map_err(|e| e.to_string())?) {
        return Err(format!("{} is not in the downloads folder", path.display()));
    }
    Ok(path)
}

#[tauri::command]
fn read_download(app: AppHandle, state: State<Shared>, path: PathBuf) -> Res<Response> {
    let path = in_downloads(&app, &state, &path)?;
    std::fs::read(&path).map(Response::new).map_err(|e| format!("{}: {e}", path.display()))
}

#[tauri::command]
fn remove_download(app: AppHandle, state: State<Shared>, path: PathBuf) -> Res<()> {
    let path = in_downloads(&app, &state, &path)?;
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether a PDF's text (from pdf.js) is reference `key`.
#[tauri::command]
fn pdf_is(app: AppHandle, state: State<Shared>, key: String, text: String) -> Res<bool> {
    state.with(&app, |b| Ok(fulltext::looks_like(&text, b.get(&key)?)))
}

#[derive(Serialize)]
struct CiteFormats {
    default: String,
    styles: Vec<&'static str>,
}

#[tauri::command]
fn cite_formats(app: AppHandle, state: State<Shared>) -> Res<CiteFormats> {
    let default = state.with(&app, |b| Ok(b.lib.cite.format.clone().unwrap_or_else(|| "bibtex".into())))?;
    Ok(CiteFormats { default, styles: yeda_core::cite::style_names() })
}

/// The references `keys` as `format` (see `cite::export`); a `.csl` path is relative to the library.
#[tauri::command]
fn cite(app: AppHandle, state: State<Shared>, keys: Vec<String>, format: String) -> Res<String> {
    state.with(&app, |b| {
        let refs: Vec<Ref> = keys.iter().filter_map(|k| b.lib.get(k).cloned()).collect();
        let dir = b.dir()?.to_path_buf();
        yeda_core::cite::export(&refs, &format, |path| Ok(std::fs::read_to_string(dir.join(path))?))
    })
}

/// Clones `url` into the empty folder `path` and makes it the library.
#[tauri::command]
async fn git_clone(app: AppHandle, state: State<'_, Shared>, url: String, path: PathBuf) -> Res<OpenRef> {
    if std::fs::read_dir(&path).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty", path.display()));
    }
    let dir = path.clone();
    let out = with_git_login(url.clone(), move |login| {
        // A failed clone can leave a half-made repository, which would block the retry.
        git::clone(&url, &dir, login).inspect_err(|_| drop(std::fs::remove_dir_all(dir.join(".git")))).map(|()| "cloned".into())
    })
    .await?;
    if matches!(out, OpenRef::Synced { .. }) {
        update_settings(&app, |s| s.library = Some(path)).map_err(err)?;
        state.change(&app, |_| Ok(()))?;
    }
    Ok(out)
}

/// The password for `dav` from the Keystore, else what `open_ref` and `attach_pdf` answer.
fn dav_password(dav: &library::Dav, rejected: bool) -> Result<String, OpenRef> {
    let account = yeda_secrets::webdav_account(&dav.user, &dav.url);
    let label = format!("{}@{}", dav.user, dav.url);
    match yeda_secrets::get(&account) {
        Ok(Some(p)) if !rejected => Ok(p),
        _ => Err(OpenRef::NeedsPassword { account, label, rejected }),
    }
}

/// Runs a WebDAV transfer off the main thread; a rejected password becomes `NeedsPassword`.
async fn transfer(
    dav: library::Dav,
    run: impl FnOnce(&library::Dav, &str) -> anyhow::Result<()> + Send + 'static,
) -> Res<Option<OpenRef>> {
    let password = match dav_password(&dav, false) {
        Ok(p) => p,
        Err(ask) => return Ok(Some(ask)),
    };
    let (dav, result) = tauri::async_runtime::spawn_blocking(move || {
        let result = run(&dav, &password);
        (dav, result)
    })
    .await
    .map_err(|e| e.to_string())?;
    match result {
        Ok(()) => Ok(None),
        Err(e) if yeda_net::webdav::is_unauthorized(&e) => Ok(dav_password(&dav, true).err()),
        Err(e) => Err(err(e)),
    }
}

/// Opens the PDF of reference `key`, with its annotations in `<library>/annotations/<key>`;
/// a PDF on WebDAV is downloaded into the cache first.
#[tauri::command]
async fn open_ref(app: AppHandle, state: State<'_, Shared>, current: State<'_, Current>, key: String) -> Res<OpenRef> {
    let (pdf, remote) = state.with(&app, |b| {
        let r = b.get(&key)?;
        let remote = b.lib.remote_pdf(r).map(|rp| (rp.dav.clone(), rp.path));
        Ok((b.lib.pdf_path(r).ok(), remote))
    })?;
    if let (Some(path), Some((dav, remote_path))) = (&pdf, remote) {
        let cached = path.clone();
        let sync = move |dav: &library::Dav, password: &str| {
            let is_current = |v: Option<&str>| Ok(yeda_net::webdav::current_version(dav, password, &remote_path)?.is_some_and(|c| Some(c.as_str()) == v));
            yeda_fs::sync_cached(&cached, is_current, || yeda_net::webdav::download(dav, password, &remote_path))
        };
        match transfer(dav, sync).await {
            Ok(Some(ask)) => return Ok(ask),
            Ok(None) => {}
            // Offline, say: the cached copy will do.
            Err(_) if path.is_file() => {}
            Err(e) => return Err(e),
        }
    }
    state.with(&app, |b| {
        let r = b.get(&key)?;
        let location = r.pdf.as_ref().map(|p| p.location.clone()).with_context(|| format!("{key} has no PDF"))?;
        match pdf {
            Some(pdf) if pdf.is_file() => {
                current.open(Doc { pdf, annotations: b.path(&library::annotations_dir(&key))?, page: None });
                Ok(OpenRef::Opened)
            }
            _ => Ok(OpenRef::MissingLocation { location }),
        }
    })
}

/// Attaches the PDF at `path` to reference `key`, copying it to `location` or uploading it there.
#[tauri::command]
async fn attach_pdf(app: AppHandle, state: State<'_, Shared>, key: String, path: PathBuf, location: String) -> Res<OpenRef> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (r, dest, remote) = state.with(&app, |b| {
        let mut r = b.get(&key)?.clone();
        r.pdf = Some(b.lib.attach(&r, &location, &path.to_string_lossy())?);
        let remote = b.lib.remote_pdf(&r).map(|rp| (rp.dav.clone(), rp.path));
        let dest = b.lib.pdf_path(&r)?;
        Ok((r, dest, remote))
    })?;
    if let Some((dav, remote_path)) = remote {
        let upload_bytes = bytes.clone();
        let cached = dest.clone();
        let upload = move |dav: &library::Dav, password: &str| {
            yeda_net::webdav::upload(dav, password, &remote_path, &upload_bytes)?;
            yeda_fs::set_cached_version(&cached, yeda_net::webdav::current_version(dav, password, &remote_path)?.as_deref())
        };
        if let Some(ask) = transfer(dav, upload).await? {
            return Ok(ask);
        }
    }
    yeda_fs::write_atomic(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    state.change(&app, |b| b.apply(vec![b.lib.save(&r)?]))?;
    Ok(OpenRef::Opened)
}

/// Opens a PDF passed through "Open with", with a sidecar annotations folder.
#[tauri::command]
fn open_pdf(current: State<Current>, path: PathBuf) {
    current.open(Doc::with_sidecar(path, None));
}

fn browser_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        library,
        set_library,
        set_location,
        llm_keys,
        set_key,
        refresh,
        list,
        parse_query,
        filter_suggestions,
        compose_query,
        details,
        annotation_image,
        set_status,
        set_tags,
        set_collection,
        set_tag_color,
        save_search,
        delete_search,
        add_ref,
        split_ids,
        delete_ref,
        save_raw,
        save_note,
        ref_pdf,
        pdf_image_regions,
        extract_annotations,
        summarize,
        export_table,
        open_ref,
        attach_pdf,
        set_secret,
        git_sync,
        git_clone,
        cite_formats,
        cite,
        fetch_plan,
        fetch_direct,
        read_download,
        remove_download,
        pdf_is,
        settings_text,
        save_settings_text,
        open_pdf
    ]
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    git::init();
    let browser = browser_handler();
    let reader = yeda_reader_lib::invoke_handler();
    tauri::Builder::default()
        .manage(Current::default())
        .manage(Shared::default())
        // Each generated handler consumes the call, so route by name instead of trying one then the other.
        .invoke_handler(move |invoke| {
            if yeda_reader_lib::COMMANDS.contains(&invoke.message.command()) { reader(invoke) } else { browser(invoke) }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
