//! yeda-android's Tauri commands: the library browser, next to yeda-reader's commands for the PDF view.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};
use yeda_core::annotations::{self, Annotation};
use yeda_core::browse::{self, Filter};
use yeda_core::library::{self, Library, Ref, config_section};
use yeda_core::search::Searcher;
use yeda_core::{Effect, tables};
use yeda_net::llm::{self, Paper, Prompt};
use yeda_net::wand;
use yeda_reader_lib::{Current, Doc};

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Per-device settings in the app's data folder; also the library's machine config, so its
/// `[locations]` override `yeda.toml` for folders that live elsewhere on this device.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    library: Option<PathBuf>,
    locations: BTreeMap<String, PathBuf>,
    /// LLM API keys by the environment variable name a profile's `api_key_env` gives.
    keys: BTreeMap<String, String>,
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
        let lib = match &dir {
            Some(d) => yeda_fs::load_library(d, Some(&settings_path(app)?), None)?,
            None => Library::default(),
        };
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
    update_settings(&app, |s| drop(s.locations.insert(name, path))).map_err(err)?;
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
    let keys = load_settings(&app).map_err(err)?.keys;
    state.with(&app, |b| {
        let mut envs: Vec<String> = b.lib.llm.values().map(|p| p.key_env().to_string()).collect();
        envs.dedup();
        Ok(envs.into_iter().map(|env| KeyStatus { set: keys.contains_key(&env), env }).collect())
    })
}

#[tauri::command]
fn set_key(app: AppHandle, env: String, key: String) -> Res<()> {
    update_settings(&app, |s| match key.trim() {
        "" => drop(s.keys.remove(&env)),
        k => drop(s.keys.insert(env, k.into())),
    })
    .map_err(err)
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
        Ok(Overview { sidebar, statuses: &library::STATUSES, tables: b.lib.tables.keys().cloned().collect() })
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
fn list(app: AppHandle, state: State<Shared>, filter: Filter, query: String) -> Res<Vec<Row>> {
    state.with(&app, |b| {
        let visible = browse::visible(&mut b.searcher, &b.lib, &filter, &query);
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

/// Without poppler, highlights come without their text and image annotations without a PNG.
// ponytail: pdf.js could supply word boxes and region renders from the WebView if that matters.
#[tauri::command]
fn extract_annotations(app: AppHandle, state: State<Shared>, key: String) -> Res<usize> {
    state.with(&app, |b| {
        let pdf = b.lib.pdf_path(b.get(&key)?)?;
        let bytes = std::fs::read(&pdf).with_context(|| pdf.display().to_string())?;
        let existing = yeda_fs::read_optional(&b.path(&annotations::file_path(&key))?)?;
        let dir = library::annotations_dir(&key);
        let (n, effects) = annotations::sync_from_pdf(&dir, existing.as_deref(), &bytes, None, &mut |_, _, _| None)?;
        b.apply(effects)?;
        Ok(n)
    })
}

/// Fills `key`'s row of `table` with the default LLM profile; returns a note on how it went.
#[tauri::command]
async fn summarize(app: AppHandle, state: State<'_, Shared>, key: String, table: String, overwrite: bool) -> Res<String> {
    let keys = load_settings(&app).map_err(err)?.keys;
    let (job, profile) = state.with(&app, |b| {
        let def = b.lib.tables.get(&table).with_context(|| format!("no table named '{table}'"))?;
        let r = b.get(&key)?;
        let job = tables::plan(&b.lib, &table, def, r, &b.annotations(&key)?, overwrite)
            .with_context(|| format!("nothing to fill in '{table}'"))?;
        Ok((job, b.lib.llm_profile(None)?.1))
    })?;
    let api_key = keys.get(profile.key_env()).cloned();
    let reply = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<_> {
        let bytes = job.pdf.as_ref().and_then(|p| std::fs::read(p).ok());
        let text = || anyhow::bail!("PDF text extraction isn't available on Android; use an Anthropic profile, which reads the PDF directly");
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
}

/// Opens the PDF of reference `key`, with its annotations in `<library>/annotations/<key>`.
#[tauri::command]
fn open_ref(app: AppHandle, state: State<Shared>, current: State<Current>, key: String) -> Res<OpenRef> {
    state.with(&app, |b| {
        let r = b.get(&key)?;
        let location = r.pdf.as_ref().map(|p| p.location.clone()).with_context(|| format!("{key} has no PDF"))?;
        match b.lib.pdf_path(r) {
            Ok(pdf) if pdf.is_file() => {
                current.open(Doc { pdf, annotations: b.path(&library::annotations_dir(&key))?, page: None });
                Ok(OpenRef::Opened)
            }
            _ => Ok(OpenRef::MissingLocation { location }),
        }
    })
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
        details,
        annotation_image,
        set_status,
        set_tags,
        set_collection,
        set_tag_color,
        save_search,
        delete_search,
        add_ref,
        delete_ref,
        save_raw,
        save_note,
        extract_annotations,
        summarize,
        export_table,
        open_ref,
        open_pdf
    ]
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
