//! The reader's Tauri commands; yeda-android registers them next to its own.

#[cfg(desktop)]
pub mod fetch;

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value as Json};
use tauri::State;
use tauri::ipc::{InvokeBody, Request, Response};
use yeda_core::annotations;

pub struct Doc {
    pub pdf: PathBuf,
    pub annotations: PathBuf,
    pub page: Option<u32>,
}

impl Doc {
    pub fn with_sidecar(pdf: PathBuf, page: Option<u32>) -> Doc {
        let mut sidecar = pdf.clone().into_os_string();
        sidecar.push(".annotations");
        Doc { pdf, annotations: sidecar.into(), page }
    }
}

/// The open document; desktop sets it from the command line, Android when a reference or file is opened.
#[derive(Default)]
pub struct Current(pub Mutex<Option<Doc>>);

impl Current {
    fn with<T>(&self, f: impl FnOnce(&Doc) -> anyhow::Result<T>) -> Result<T, String> {
        let doc = self.0.lock().unwrap();
        f(doc.as_ref().ok_or("no document open")?).map_err(|e| format!("{e:#}"))
    }

    pub fn open(&self, doc: Doc) {
        *self.0.lock().unwrap() = Some(doc);
    }
}

/// Every command `invoke_handler` registers.
pub const COMMANDS: &[&str] =
    &["launch", "read_pdf", "load_annotations", "save_annotations", "delete_annotations", "read_image", "save_image"];

pub fn invoke_handler<R: tauri::Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![launch, read_pdf, load_annotations, save_annotations, delete_annotations, read_image, save_image]
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
    current.with(|doc| std::fs::read(&doc.pdf).map(Response::new).map_err(|e| anyhow::anyhow!("{}: {e}", doc.pdf.display())))
}

fn annotations_file(doc: &Doc) -> PathBuf {
    doc.annotations.join(annotations::FILE)
}

#[tauri::command]
fn load_annotations(current: State<Current>) -> Result<Vec<Json>, String> {
    current.with(|doc| annotations::to_json(yeda_fs::read_optional(&annotations_file(doc))?.as_deref()))
}

/// Re-reads the file right before merging, since yeda-tui may have written it since.
#[tauri::command]
fn save_annotations(current: State<Current>, annotations: Vec<Map<String, Json>>) -> Result<(), String> {
    current.with(|doc| {
        yeda_fs::locked(&doc.annotations, || {
            let path = annotations_file(doc);
            let merged = annotations::merge(yeda_fs::read_optional(&path)?.as_deref(), annotations)?;
            Ok(yeda_fs::write_atomic(&path, merged.as_bytes())?)
        })
    })
}

#[tauri::command]
fn delete_annotations(current: State<Current>, ids: Vec<String>) -> Result<(), String> {
    current.with(|doc| {
        yeda_fs::locked(&doc.annotations, || {
            let path = annotations_file(doc);
            let (kept, images) = annotations::delete(yeda_fs::read_optional(&path)?.as_deref(), &ids)?;
            yeda_fs::write_atomic(&path, kept.as_bytes())?;
            let effects = images.into_iter().map(yeda_core::Effect::remove);
            yeda_fs::apply(&doc.annotations, effects)
        })
    })
}

fn image_path(doc: &Doc, id: &str) -> anyhow::Result<PathBuf> {
    Ok(doc.annotations.join(annotations::image_name(id)?))
}

#[tauri::command]
fn read_image(current: State<Current>, id: String) -> Result<Response, String> {
    current.with(|doc| Ok(Response::new(std::fs::read(image_path(doc, &id)?)?)))
}

/// Takes the PNG as the raw request body and the annotation id in the `id` header.
#[tauri::command]
fn save_image(current: State<Current>, request: Request) -> Result<(), String> {
    // Android's postMessage IPC has no raw bodies; it sends the bytes as a JSON array.
    let png = match request.body() {
        InvokeBody::Raw(png) => png.clone(),
        InvokeBody::Json(v) => serde_json::from_value(v.clone()).map_err(|_| "expected PNG bytes")?,
    };
    let id = request.headers().get("id").and_then(|v| v.to_str().ok()).ok_or("missing id header")?;
    current.with(|doc| Ok(yeda_fs::write_atomic(&image_path(doc, id)?, &png)?))
}

const USAGE: &str = "usage: yeda-reader <file.pdf> [--annotations <dir>] [--page N]";

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

pub fn run() {
    run_with(std::env::args().skip(1))
}

/// Runs the reader with these command-line arguments (without the program name).
pub fn run_with(args: impl Iterator<Item = String>) {
    let doc = parse_args(args).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    let current = Current::default();
    current.open(doc);
    tauri::Builder::default()
        .manage(current)
        .invoke_handler(invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sidecar_defaults_next_to_pdf() {
        let doc = parse_args(["a/b.pdf".to_string(), "--page".into(), "3".into()].into_iter()).unwrap();
        assert_eq!(doc.annotations, Path::new("a/b.pdf.annotations"));
        assert_eq!(doc.page, Some(3));
        let doc = parse_args(["b.pdf".to_string(), "--annotations".into(), "lib/annotations/k".into()].into_iter()).unwrap();
        assert_eq!(doc.annotations, Path::new("lib/annotations/k"));
    }
}
