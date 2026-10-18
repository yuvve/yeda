//! `yeda fetch`: a browser window that fetches PDFs through the library proxy. You sign in and
//! click through where needed; every download lands in the downloads folder. Reads a `Job` as
//! JSON on stdin and prints one JSON `Outcome` per paper on stdout.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::webview::DownloadEvent;
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use yeda_core::fulltext::{self, Settings};

#[derive(Deserialize)]
pub struct Job {
    /// Absolute; downloads are saved here.
    pub downloads: PathBuf,
    pub settings: Settings,
    pub papers: Vec<Paper>,
}

#[derive(Deserialize, Clone)]
pub struct Paper {
    pub key: String,
    pub doi: String,
    /// The file name to save the PDF as.
    pub file: String,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Saved { key: String, file: PathBuf },
    Failed { key: String, error: String },
    /// A page the window loaded, for the log.
    Visited { key: String, page: String },
}

enum Event {
    Loaded(String),
    /// A page asked for a new window, which the fetch window opens itself instead.
    NewWindow(String),
    /// The page's title; the script reports through it, as pages from the web get no IPC.
    Title(String),
    Downloaded(Option<PathBuf>),
    Skip,
    Stop,
}

fn report(outcome: &Outcome) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, outcome);
    let _ = writeln!(out);
    let _ = out.flush();
}

const HTML: &str = "yeda:html ";

/// Reports the page and its `citation_pdf_url` in the title, the link through the proxy when the
/// page came through it. WebKit's PDF viewer runs no scripts, so a page that never reports is a PDF.
fn report_script(settings: &Settings) -> String {
    let proxy = serde_json::to_string(&settings.proxy).unwrap();
    let host = serde_json::to_string(&fulltext::proxy_host(settings)).unwrap();
    format!(
        r#"(() => {{
  const m = document.querySelector('meta[name="citation_pdf_url"]');
  let url = m && m.content ? new URL(m.content, location.href).href : '';
  const proxy = {proxy}, host = {host};
  if (url && host && location.hostname.endsWith('.' + host) && !new URL(url).hostname.endsWith('.' + host)) url = proxy.replace('{{url}}', url);
  document.title = '{HTML}' + location.href + ' ' + url;
}})()"#
    )
}

/// Has the page download `url` as a click on a link would, so the browser's own session and
/// checks apply; a link to another site opens instead.
fn download_in_page(window: &WebviewWindow, url: &str) {
    let url = serde_json::to_string(url).unwrap();
    let _ = window.eval(format!("(() => {{ const a = document.createElement('a'); a.href = {url}; a.download = ''; document.body.appendChild(a); a.click(); }})()"));
}

/// Has WebKit download `url` itself, which reaches `on_download` like any download.
fn download_natively(window: &WebviewWindow, url: &str) {
    // ponytail: WebKitGTK only; elsewhere the viewer's own download button remains.
    #[cfg(target_os = "linux")]
    {
        use webkit2gtk::WebViewExt;
        let url = url.to_string();
        let _ = window.with_webview(move |w| drop(w.inner().download_uri(&url)));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (window, url);
}

/// The PDF at `url`, downloaded with the window's cookies for it.
fn fetch_with_cookies(window: &WebviewWindow, url: &str) -> Option<Vec<u8>> {
    let cookies = window.cookies_for_url(url.parse().ok()?).ok()?;
    let header = cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ");
    yeda_net::fulltext::download(url, Some(&header).filter(|h| !h.is_empty()).map(String::as_str)).ok()
}

pub fn run(job: Job) {
    let (tx, rx) = mpsc::channel::<Event>();
    // The file name of the paper being fetched, so `on_download` knows where to save.
    let current: Arc<Mutex<String>> = Arc::default();
    let mut context = tauri::generate_context!();
    // The reader's config opens its own window; this mode makes its own.
    context.config_mut().app.windows.clear();
    let downloads = job.downloads.clone();
    tauri::Builder::default()
        .setup(move |app| {
            std::fs::create_dir_all(&downloads)?;
            let menu = Menu::with_items(
                app,
                &[&Submenu::with_items(
                    app,
                    "Fetch",
                    true,
                    &[&MenuItem::with_id(app, "skip", "Skip this paper", true, None::<&str>)?, &MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?],
                )?],
            )?;
            let (load_tx, dl_tx, menu_tx, title_tx, new_tx) = (tx.clone(), tx.clone(), tx.clone(), tx.clone(), tx.clone());
            let target = current.clone();
            let dir = downloads.clone();
            let window = WebviewWindowBuilder::new(app, "fetch", WebviewUrl::External("about:blank".parse().unwrap()))
                .title("yeda fetch")
                .inner_size(1100.0, 850.0)
                .menu(menu)
                // A profile of its own keeps the proxy session between runs.
                .data_directory(app.path().app_data_dir()?.join("fetch-browser"))
                .on_document_title_changed(move |_, title| drop(title_tx.send(Event::Title(title))))
                .on_new_window(move |url, _| {
                    let _ = new_tx.send(Event::NewWindow(url.to_string()));
                    tauri::webview::NewWindowResponse::Deny
                })
                .on_page_load(move |_, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        let _ = load_tx.send(Event::Loaded(payload.url().to_string()));
                    }
                })
                .on_download(move |_, event| {
                    match event {
                        DownloadEvent::Requested { destination, .. } => {
                            *destination = dir.join(format!("{}.part", target.lock().unwrap()));
                        }
                        DownloadEvent::Finished { path, success, .. } => {
                            let _ = dl_tx.send(Event::Downloaded(path.filter(|_| success)));
                        }
                        _ => {}
                    }
                    true
                })
                .build()?;
            app.on_menu_event(move |_, e| {
                let _ = menu_tx.send(if e.id() == "skip" { Event::Skip } else { Event::Stop });
            });
            let close_tx = tx.clone();
            window.on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { .. } = e {
                    let _ = close_tx.send(Event::Stop);
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                drive(&window, &job, &rx, &current);
                handle.exit(0);
            });
            Ok(())
        })
        .run(context)
        .expect("error while running the fetch window");
}

fn drive(window: &WebviewWindow, job: &Job, rx: &Receiver<Event>, current: &Mutex<String>) {
    let script = report_script(&job.settings);
    for (n, paper) in job.papers.iter().enumerate() {
        *current.lock().unwrap() = paper.file.clone();
        let _ = window.set_title(&format!("yeda fetch {}/{} · {} — sign in or click the PDF link if it stops", n + 1, job.papers.len(), paper.key));
        if let Some(file) = without_window(window, job, paper) {
            report(&Outcome::Saved { key: paper.key.clone(), file });
            continue;
        }
        // Events from the last paper's page (a repeated report, a late download) would be taken as this one's.
        if !blank_page(window, rx) {
            for p in &job.papers[n..] {
                report(&Outcome::Failed { key: p.key.clone(), error: "fetch window closed".into() });
            }
            return;
        }
        let url = fulltext::proxied(&format!("https://doi.org/{}", paper.doi), &job.settings);
        if let Err(e) = url.parse().map_err(anyhow::Error::from).and_then(|u| Ok(window.navigate(u)?)) {
            report(&Outcome::Failed { key: paper.key.clone(), error: format!("{e:#}") });
            continue;
        }
        let target = job.downloads.join(&paper.file);
        let part = job.downloads.join(format!("{}.part", paper.file));
        let save = |pdf: Vec<u8>| std::fs::write(&target, pdf).ok().map(|()| Outcome::Saved { key: paper.key.clone(), file: target.clone() });
        // The page waiting for its script to report, and the PDF links already tried.
        let mut loaded: Option<String> = None;
        let mut tried: Vec<String> = Vec::new();
        let outcome = loop {
            let event = match loaded {
                Some(_) => rx.recv_timeout(Duration::from_secs(2)),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(Event::Loaded(url)) => {
                    if url != "about:blank" {
                        report(&Outcome::Visited { key: paper.key.clone(), page: without_query(&url) });
                    }
                    // IEEE's pages fail their bot check, or show the PDF in a frame, which fires no download.
                    if let Some(pdf) = fulltext::ieee_pdf(&url).filter(|p| !tried.contains(p)) {
                        tried.push(pdf.clone());
                        match fetch_with_cookies(window, &pdf).and_then(save) {
                            Some(outcome) => break outcome,
                            None => download_in_page(window, &pdf),
                        }
                    }
                    let _ = window.eval(&script);
                    loaded = Some(url);
                }
                Ok(Event::Title(title)) => {
                    let Some((page, pdf)) = title.strip_prefix(HTML).and_then(|t| t.split_once(' ')) else { continue };
                    // A report from a page already left (a redirect) mustn't cancel the wait for this one.
                    if loaded.as_deref().is_some_and(|l| without_fragment(l) == without_fragment(page)) {
                        loaded = None;
                    }
                    if pdf.is_empty() || tried.iter().any(|t| t == pdf) {
                        continue;
                    }
                    tried.push(pdf.to_string());
                    match fetch_with_cookies(window, pdf).and_then(save) {
                        Some(outcome) => break outcome,
                        // The browser may get what a plain request can't; a link to another site
                        // just opens, showing what stands in the way: a sign-in or a check.
                        None => download_in_page(window, pdf),
                    }
                }
                // No report: WebKit's PDF viewer, which can be downloaded from like any page.
                Err(RecvTimeoutError::Timeout) => {
                    let url = loaded.take().unwrap_or_default();
                    if let Some(outcome) = fetch_with_cookies(window, &url).and_then(save) {
                        break outcome;
                    }
                    // Signed PDF links (ScienceDirect's) may refuse a plain request; WebKit's own
                    // download, as from the viewer's button, carries the browser's session.
                    if !url.is_empty() && !tried.contains(&url) {
                        tried.push(url.clone());
                        report(&Outcome::Visited { key: paper.key.clone(), page: format!("WebKit download of {}", without_query(&url)) });
                        download_natively(window, &url);
                    }
                }
                // Anything but a PDF (an HTML page, a cancelled download) waits for the next one.
                Ok(Event::Downloaded(Some(done))) if done == part => {
                    if let Some(file) = keep_pdf(&part, &target) {
                        break Outcome::Saved { key: paper.key.clone(), file };
                    }
                }
                Ok(Event::Downloaded(_)) => {}
                Ok(Event::NewWindow(url)) => drop(url.parse().map(|u| window.navigate(u))),
                Ok(Event::Skip) => break Outcome::Failed { key: paper.key.clone(), error: "skipped in the browser".into() },
                Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    for p in &job.papers[n..] {
                        report(&Outcome::Failed { key: p.key.clone(), error: "fetch window closed".into() });
                    }
                    return;
                }
            }
        };
        report(&outcome);
    }
}

/// Loads an empty page and drops every event until it has; false if the window was closed.
fn blank_page(window: &WebviewWindow, rx: &Receiver<Event>) -> bool {
    if window.navigate("about:blank".parse().unwrap()).is_err() {
        return false;
    }
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::Loaded(url)) if url == "about:blank" => return true,
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return false,
            // ponytail: a blank page that never reports loading is waited for once, then ignored.
            Err(RecvTimeoutError::Timeout) => return true,
            Ok(_) => {}
        }
    }
}

fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or_default()
}

/// `url` without its query and fragment, which can hold sign-in tickets.
fn without_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or_default().to_string()
}

/// Renames a finished download to `target` if it is a PDF; anything else is deleted.
fn keep_pdf(part: &std::path::Path, target: &std::path::Path) -> Option<PathBuf> {
    let bytes = std::fs::read(part).ok()?;
    if !fulltext::is_pdf(&bytes) {
        let _ = std::fs::remove_file(part);
        return None;
    }
    std::fs::rename(part, target).ok()?;
    Some(target.to_path_buf())
}

/// With an open proxy session, publishers with a known PDF URL need no page at all.
fn without_window(window: &WebviewWindow, job: &Job, paper: &Paper) -> Option<PathBuf> {
    let host = fulltext::proxy_host(&job.settings)?;
    for url in fulltext::publisher_urls(&paper.doi, &job.settings) {
        let url = fulltext::proxied_host(&url, &host)?;
        let cookies = window.cookies_for_url(url.parse().ok()?).ok()?;
        if cookies.is_empty() {
            return None;
        }
        let header = cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ");
        if let Ok(pdf) = yeda_net::fulltext::download(&url, Some(&header)) {
            let target = job.downloads.join(&paper.file);
            std::fs::write(&target, pdf).ok()?;
            return Some(target);
        }
    }
    None
}
