//! yeda-android's Tauri commands: the library browser, next to yeda-reader's commands for the PDF view.

mod git;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};
use yeda_core::annotations::{self, Annotation};
use yeda_core::browse::{self, Filter, Sort, SortField};
use yeda_core::library::{self, Library, Ref, config_section};
use yeda_core::search::{self, FILTER_FIELDS, Searcher};
use yeda_core::Effect;
use yeda_reader_lib::{Current, Doc};

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// The device config, in the app's data folder (`settings.toml`): the library folder, and
/// `[locations]` for folders that live elsewhere on this device. Secrets go in the Keystore, never here.
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

/// Replaces `settings.toml`, once it parses both as settings and as a device config.
#[tauri::command]
fn save_settings_text(app: AppHandle, state: State<Shared>, text: String) -> Res<()> {
    toml::from_str::<Settings>(&text).map_err(|e| e.to_string())?;
    Library::parse(None, Some(&text), [], None).map_err(err)?;
    yeda_fs::write_atomic(&settings_path(&app).map_err(err)?, text.as_bytes()).map_err(|e| e.to_string())?;
    state.change(&app, |_| Ok(()))
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
    /// A saved search's order.
    sort: Option<Sort>,
}

#[derive(Serialize)]
struct Overview {
    sidebar: Vec<SideEntry>,
    statuses: &'static [&'static str],
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
                let sort = match &filter {
                    Filter::Search(name, _) => b.lib.searches.get(name).and_then(|s| s.order()),
                    _ => None,
                };
                SideEntry { filter, color, sort }
            })
            .collect();
        Ok(Overview {
            sidebar,
            statuses: &library::STATUSES,
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
}

#[tauri::command]
fn details(app: AppHandle, state: State<Shared>, key: String) -> Res<Details> {
    state.with(&app, |b| {
        let reference = b.get(&key)?.clone();
        let raw = yeda_fs::read_optional(&b.path(&library::ref_path(&key))?)?.unwrap_or_default();
        let note = yeda_fs::read_optional(&b.path(&library::note_path(&key))?)?.unwrap_or_default();
        Ok(Details { annotations: b.annotations(&key)?, key, reference, raw, note })
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
/// Saves a search with its order (`None`: unsorted), replacing `old` when it is renamed.
fn save_search(app: AppHandle, state: State<Shared>, name: String, query: String, sort: Option<Sort>, old: Option<String>) -> Res<()> {
    let saved = library::SavedSearch { query, sort: sort.map(|s| s.field), descending: sort.is_some_and(|s| s.descending) };
    state.change(&app, |b| {
        let effect = b.lib.save_search(&name, &saved, old.as_deref().filter(|o| *o != name))?;
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

/// Tests a WebDAV login, then keeps the password in the Keystore and `[locations.<name>]` in yeda.toml.
#[tauri::command]
async fn setup_webdav(app: AppHandle, state: State<'_, Shared>, name: String, url: String, user: String, password: String) -> Res<()> {
    let (name, url, user) = (name.trim().to_string(), url.trim().to_string(), user.trim().to_string());
    if name.is_empty() || user.is_empty() || password.is_empty() || !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("give a name, an https:// URL, the user name and the password".into());
    }
    let dav = library::Dav { url, user };
    let login = dav.clone();
    let pw = password.clone();
    tauri::async_runtime::spawn_blocking(move || yeda_net::webdav::login(&login, &pw)).await.map_err(|e| e.to_string())?.map_err(err)?;
    yeda_secrets::set(&yeda_secrets::webdav_account(&dav.user, &dav.url), &password).map_err(err)?;
    state.change(&app, |b| {
        let effect = b.lib.update_config(|t| {
            let section = toml::Table::from_iter([("url".to_string(), dav.url.into()), ("user".to_string(), dav.user.into())]);
            config_section(t, "locations")?.insert(name, section.into());
            Ok(())
        })?;
        b.apply(vec![effect])
    })
}

fn empty(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let _ = match entry.file_type() {
            Ok(t) if t.is_dir() => std::fs::remove_dir_all(entry.path()),
            _ => std::fs::remove_file(entry.path()),
        };
    }
}

/// Clones `url` into the empty folder `path` and makes it the library.
#[tauri::command]
async fn git_clone(app: AppHandle, state: State<'_, Shared>, url: String, path: PathBuf) -> Res<OpenRef> {
    if std::fs::read_dir(&path).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty", path.display()));
    }
    let dir = path.clone();
    let out = with_git_login(url.clone(), move |login| {
        // The folder was empty, and what a failed clone leaves in it would block the retry.
        git::clone(&url, &dir, login).inspect_err(|_| empty(&dir)).map(|()| "cloned".into())
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
        setup_webdav,
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
        delete_ref,
        save_raw,
        save_note,
        ref_pdf,
        pdf_image_regions,
        extract_annotations,
        open_ref,
        attach_pdf,
        set_secret,
        git_sync,
        git_clone,
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
