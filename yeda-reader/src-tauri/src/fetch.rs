//! `yeda fetch`: a browser window that fetches PDFs through the library proxy. You sign in and
//! click through where needed; every download lands in the downloads folder. Reads a `Job` as
//! JSON on stdin and prints one JSON `Outcome` per paper on stdout.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
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
    Downloaded(PathBuf),
    NextProxy,
    Skip,
    Stop,
}

/// How trying a paper through one proxy ended.
enum Step {
    Done(Outcome),
    NextProxy,
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
fn report_script(proxy: Option<&str>) -> String {
    let host = serde_json::to_string(&proxy.and_then(fulltext::proxy_host)).unwrap();
    let proxy = serde_json::to_string(&proxy).unwrap();
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
            let fetch = Submenu::with_items(
                app,
                "Fetch",
                true,
                &[&MenuItem::with_id(app, "skip", "Skip this paper", true, None::<&str>)?, &MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?],
            )?;
            if job.settings.proxies.len() > 1 {
                fetch.prepend(&MenuItem::with_id(app, "next", "Try the next proxy", true, None::<&str>)?)?;
            }
            let menu = Menu::with_items(app, &[&fetch])?;
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
                        // wry 0.57 shares one failure flag among all downloads and never clears it, so
                        // after one failure `success` stays false; `keep_pdf` checks the file instead.
                        DownloadEvent::Finished { .. } => {
                            let _ = dl_tx.send(Event::Downloaded(dir.join(format!("{}.part", target.lock().unwrap()))));
                        }
                        _ => {}
                    }
                    true
                })
                .build()?;
            app.on_menu_event(move |_, e| {
                let _ = menu_tx.send(match e.id().as_ref() {
                    "next" => Event::NextProxy,
                    "skip" => Event::Skip,
                    _ => Event::Stop,
                });
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
    let proxies: Vec<Option<&str>> = match job.settings.proxies.as_slice() {
        [] => vec![None],
        list => list.iter().map(|p| Some(p.as_str())).collect(),
    };
    for (n, paper) in job.papers.iter().enumerate() {
        *current.lock().unwrap() = paper.file.clone();
        let mut outcome = Outcome::Failed { key: paper.key.clone(), error: "no proxy gave a PDF".into() };
        for (i, proxy) in proxies.iter().enumerate() {
            let through = if proxies.len() > 1 { format!(" · proxy {}/{}", i + 1, proxies.len()) } else { String::new() };
            let _ = window.set_title(&format!("yeda fetch {}/{} · {}{through} — sign in or click the PDF link if it stops", n + 1, job.papers.len(), paper.key));
            match through_proxy(window, job, paper, *proxy, rx) {
                Step::Done(o) => {
                    outcome = o;
                    break;
                }
                Step::NextProxy => {}
                Step::Stop => {
                    for p in &job.papers[n..] {
                        report(&Outcome::Failed { key: p.key.clone(), error: "fetch window closed".into() });
                    }
                    return;
                }
            }
        }
        report(&outcome);
    }
}

/// Opens `paper` through `proxy` and waits for its PDF, or for the user to move on. The browser
/// makes every request: a publisher's bot check ties its cookies to the browser that earned
/// them, and the same cookies sent by another client count as a failed check.
fn through_proxy(window: &WebviewWindow, job: &Job, paper: &Paper, proxy: Option<&str>, rx: &Receiver<Event>) -> Step {
    // Events from the last page (a repeated report, a late download) would be taken as this one's.
    if !blank_page(window, rx) {
        return Step::Stop;
    }
    let target = job.downloads.join(&paper.file);
    let part = job.downloads.join(format!("{}.part", paper.file));
    let saved = |file: PathBuf| Step::Done(Outcome::Saved { key: paper.key.clone(), file });
    // Signed in already: a publisher's known PDF URL may need no page at all.
    for url in signed_in_urls(window, job, paper, proxy) {
        download_natively(window, &url);
        match wait_for_download(rx, &part, &target, paper) {
            Some(step) => return step,
            None => continue,
        }
    }
    let doi = format!("https://doi.org/{}", paper.doi);
    let url = proxy.map_or(doi.clone(), |p| fulltext::proxied(&doi, p));
    if let Err(e) = url.parse().map_err(anyhow::Error::from).and_then(|u| Ok(window.navigate(u)?)) {
        return Step::Done(Outcome::Failed { key: paper.key.clone(), error: format!("{e:#}") });
    }
    let script = report_script(proxy);
    // The page waiting for its script to report, and the links already tried.
    let mut loaded: Option<String> = None;
    let mut tried: Vec<String> = Vec::new();
    loop {
        let event = match loaded {
            Some(_) => rx.recv_timeout(Duration::from_secs(2)),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match event {
            Ok(Event::Loaded(url)) => {
                if url != "about:blank" {
                    report(&Outcome::Visited { key: paper.key.clone(), page: without_query(&url) });
                }
                // ScienceDirect's PDF link is an intermediate page that ends in the viewer, and IEEE's
                // document pages don't render here; its PDF page is what the PDF button opens.
                let next = match (fulltext::sciencedirect_pdf(&url), fulltext::ieee(&url)) {
                    (Some(pdf), _) => Some(pdf),
                    (_, Some(fulltext::Ieee::Document { stamp })) => Some(stamp),
                    _ => None,
                };
                if let Some(next) = next.filter(|n| !tried.contains(n)) {
                    tried.push(next.clone());
                    let _ = next.parse().map(|u| window.navigate(u));
                    // Left at once, so this page's report isn't waited for.
                    loaded = None;
                    continue;
                }
                // On every load, so a bot check passed on the PDF page is followed by the PDF.
                if let Some(fulltext::Ieee::Stamp { pdf }) = fulltext::ieee(&url) {
                    download_natively(window, &pdf);
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
                // A link to another site just opens, showing what stands in the way: a sign-in or a check.
                download_in_page(window, pdf);
            }
            // No report: WebKit's PDF viewer, which can be downloaded from like any page.
            Err(RecvTimeoutError::Timeout) => {
                let url = loaded.take().unwrap_or_default();
                if !url.is_empty() && !tried.contains(&url) {
                    tried.push(url.clone());
                    report(&Outcome::Visited { key: paper.key.clone(), page: format!("WebKit download of {}", without_query(&url)) });
                    download_natively(window, &url);
                }
            }
            // Anything but a PDF (an HTML page, a cancelled download) waits for the next one.
            Ok(Event::Downloaded(done)) if done == part => {
                if let Some(file) = keep_pdf_or_report(&part, &target, paper) {
                    return saved(file);
                }
            }
            Ok(Event::Downloaded(_)) => {}
            Ok(Event::NewWindow(url)) => drop(url.parse().map(|u| window.navigate(u))),
            Ok(Event::NextProxy) => return Step::NextProxy,
            Ok(Event::Skip) => return skipped(paper),
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return Step::Stop,
        }
    }
}

fn skipped(paper: &Paper) -> Step {
    Step::Done(Outcome::Failed { key: paper.key.clone(), error: "skipped in the browser".into() })
}

/// The publisher's known PDF URLs, through `proxy`, for which the window holds that proxy's cookies.
fn signed_in_urls(window: &WebviewWindow, job: &Job, paper: &Paper, proxy: Option<&str>) -> Vec<String> {
    let Some(host) = proxy.and_then(fulltext::proxy_host) else { return Vec::new() };
    let signed_in = |url: &String| url.parse().ok().and_then(|u| window.cookies_for_url(u).ok()).is_some_and(|c| !c.is_empty());
    fulltext::publisher_urls(&paper.doi, &job.settings).iter().filter_map(|u| fulltext::proxied_host(u, &host)).filter(signed_in).collect()
}

/// Waits up to half a minute for the download of `part`; the step when it ended the paper (saved,
/// skipped, stopped), `None` when nothing usable came.
fn wait_for_download(rx: &Receiver<Event>, part: &Path, target: &Path, paper: &Paper) -> Option<Step> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Event::Downloaded(done)) if done == part => {
                return keep_pdf_or_report(part, target, paper).map(|file| Step::Done(Outcome::Saved { key: paper.key.clone(), file }));
            }
            Ok(Event::Skip) => return Some(skipped(paper)),
            Ok(Event::NextProxy) => return Some(Step::NextProxy),
            Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return Some(Step::Stop),
            Err(RecvTimeoutError::Timeout) => return None,
            Ok(_) => {}
        }
    }
}

/// `keep_pdf`, reporting what came instead of a PDF: a bot check or an access page, say.
fn keep_pdf_or_report(part: &Path, target: &Path, paper: &Paper) -> Option<PathBuf> {
    let bytes = std::fs::read(part).unwrap_or_default();
    if !bytes.is_empty() && !fulltext::is_pdf(&bytes) {
        let gist = fulltext::page_gist(&String::from_utf8_lossy(&bytes[..bytes.len().min(512 << 10)]));
        report(&Outcome::Visited { key: paper.key.clone(), page: format!("downloaded a page, not a PDF: {gist}") });
    }
    keep_pdf(part, target)
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

/// Renames a finished download to `target` if it is a whole PDF; anything else is deleted.
fn keep_pdf(part: &Path, target: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(part).ok()?;
    let tail = &bytes[bytes.len().saturating_sub(2048)..];
    if !fulltext::is_pdf(&bytes) || !tail.windows(5).any(|w| w == b"%%EOF") {
        let _ = std::fs::remove_file(part);
        return None;
    }
    std::fs::rename(part, target).ok()?;
    Some(target.to_path_buf())
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_only_whole_pdfs() {
        let dir = std::env::temp_dir().join(format!("yeda-keep-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (part, target) = (dir.join("x.pdf.part"), dir.join("x.pdf"));
        std::fs::write(&part, b"%PDF-1.7\n1 0 obj").unwrap();
        assert!(super::keep_pdf(&part, &target).is_none());
        assert!(!part.exists());
        std::fs::write(&part, b"%PDF-1.7\n1 0 obj\nendobj\n%%EOF\n").unwrap();
        assert_eq!(super::keep_pdf(&part, &target), Some(target.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
