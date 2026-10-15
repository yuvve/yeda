mod annotations;
mod library;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value as Json};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager, State};

use library::{Config, Resolved};

struct Doc {
    pdf: PathBuf,
    annotations: PathBuf,
    page: Option<u32>,
}

impl Doc {
    fn with_sidecar(pdf: PathBuf, page: Option<u32>) -> Doc {
        let mut sidecar = pdf.clone().into_os_string();
        sidecar.push(".annotations");
        Doc { pdf, annotations: sidecar.into(), page }
    }
}

/// The open document; desktop sets it from the command line, Android from the start screen or "Open with".
#[derive(Default)]
struct Current(Mutex<Option<Doc>>);

impl Current {
    fn with<T>(&self, f: impl FnOnce(&Doc) -> Result<T, String>) -> Result<T, String> {
        f(self.0.lock().unwrap().as_ref().ok_or("no document open")?)
    }
}

#[cfg(desktop)]
const USAGE: &str = "usage: yeda-reader <file.pdf> [--annotations <dir>] [--page N]";

#[cfg(desktop)]
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Doc, String> {
    let mut pdf = None;
    let mut annotations = None;
    let mut page = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--annotations" => annotations = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            "--page" => {
                let n = args.next().ok_or(USAGE)?;
                page = Some(n.parse().map_err(|_| format!("invalid page: {n}"))?);
            }
            _ if pdf.is_none() && !arg.starts_with("--") => pdf = Some(PathBuf::from(arg)),
            _ => return Err(USAGE.into()),
        }
    }
    let doc = Doc::with_sidecar(pdf.ok_or(USAGE)?, page);
    Ok(Doc { annotations: annotations.unwrap_or(doc.annotations), ..doc })
}

#[derive(Serialize)]
struct Launch {
    title: String,
    page: Option<u32>,
}

#[tauri::command]
fn launch(current: State<Current>) -> Option<Launch> {
    current
        .with(|doc| {
            let title = doc.pdf.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            Ok(Launch { title, page: doc.page })
        })
        .ok()
}

#[tauri::command]
fn read_pdf(current: State<Current>) -> Result<Response, String> {
    current.with(|doc| {
        std::fs::read(&doc.pdf).map(Response::new).map_err(|e| format!("{}: {e}", doc.pdf.display()))
    })
}

#[tauri::command]
fn load_annotations(current: State<Current>) -> Result<Vec<Json>, String> {
    current.with(|doc| annotations::load(&doc.annotations))
}

#[tauri::command]
fn save_annotations(current: State<Current>, annotations: Vec<Map<String, Json>>) -> Result<(), String> {
    current.with(|doc| annotations::save(&doc.annotations, annotations))
}

#[tauri::command]
fn delete_annotations(current: State<Current>, ids: Vec<String>) -> Result<(), String> {
    current.with(|doc| annotations::delete(&doc.annotations, &ids))
}

#[tauri::command]
fn read_image(current: State<Current>, id: String) -> Result<Response, String> {
    current.with(|doc| annotations::read_image(&doc.annotations, &id).map(Response::new))
}

/// Takes the PNG as the raw request body and the annotation id in the `id` header.
#[tauri::command]
fn save_image(current: State<Current>, request: Request) -> Result<(), String> {
    let InvokeBody::Raw(png) = request.body() else { return Err("expected raw PNG body".into()) };
    let id = request.headers().get("id").and_then(|v| v.to_str().ok()).ok_or("missing id header")?;
    current.with(|doc| annotations::save_image(&doc.annotations, id, png))
}

#[tauri::command]
fn open_pdf(current: State<Current>, path: PathBuf) {
    *current.0.lock().unwrap() = Some(Doc::with_sidecar(path, None));
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("config.json"))
}

fn library_dir(config: &Config) -> Result<&Path, String> {
    config.library.as_deref().ok_or_else(|| "no library folder chosen".into())
}

#[tauri::command]
fn library(app: AppHandle) -> Result<Option<PathBuf>, String> {
    Ok(Config::load(&config_path(&app)?).library)
}

#[tauri::command]
fn set_library(app: AppHandle, path: PathBuf) -> Result<(), String> {
    let path_ = config_path(&app)?;
    let mut config = Config::load(&path_);
    config.library = Some(path);
    config.save(&path_)
}

#[tauri::command]
fn set_location(app: AppHandle, name: String, path: PathBuf) -> Result<(), String> {
    let path_ = config_path(&app)?;
    let mut config = Config::load(&path_);
    config.locations.insert(name, path);
    config.save(&path_)
}

#[tauri::command]
fn list_refs(app: AppHandle) -> Result<Vec<library::RefSummary>, String> {
    library::list_refs(library_dir(&Config::load(&config_path(&app)?))?)
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum OpenRef {
    Opened,
    MissingLocation { location: String },
}

/// Opens the PDF of reference `key`, with its annotations in `<library>/annotations/<key>`.
#[tauri::command]
fn open_ref(app: AppHandle, current: State<Current>, key: String) -> Result<OpenRef, String> {
    let config = Config::load(&config_path(&app)?);
    let library = library_dir(&config)?;
    Ok(match library::resolve_pdf(library, &key, &config)? {
        Resolved::Pdf(pdf) => {
            let annotations = library.join("annotations").join(&key);
            *current.0.lock().unwrap() = Some(Doc { pdf, annotations, page: None });
            OpenRef::Opened
        }
        Resolved::MissingLocation(location) => OpenRef::MissingLocation { location },
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let current = Current::default();
    #[cfg(desktop)]
    {
        let doc = parse_args(std::env::args().skip(1)).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        });
        *current.0.lock().unwrap() = Some(doc);
    }
    tauri::Builder::default()
        .manage(current)
        .invoke_handler(tauri::generate_handler![
            launch,
            read_pdf,
            load_annotations,
            save_annotations,
            delete_annotations,
            read_image,
            save_image,
            open_pdf,
            library,
            set_library,
            set_location,
            list_refs,
            open_ref
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
