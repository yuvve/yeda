use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use yeda_core::fulltext::{self, Settings};
use yeda_net::openalex::Client;

pub struct Item {
    pub key: String,
    pub doi: Option<String>,
    /// For references without a DOI, which OpenAlex may still know by title.
    pub openalex: Option<String>,
    pub file: String,
}

pub struct Request {
    pub items: Vec<Item>,
    pub settings: Settings,
    pub downloads: PathBuf,
    pub openalex_key: Option<String>,
}

pub enum Msg {
    Progress(String),
    /// `verify`: the file came from the browser, where any link could have been clicked.
    Saved { key: String, file: PathBuf, verify: bool },
    Failed { key: String, reason: String },
    Done,
}

// Polite to arXiv, which asks for about one request every 3 s, and to publishers.
const PAUSE: Duration = Duration::from_secs(3);

pub fn spawn(req: Request) -> Receiver<Msg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        run(req, &tx);
        let _ = tx.send(Msg::Done);
    });
    rx
}

fn run(req: Request, tx: &Sender<Msg>) {
    let send = |m: Msg| drop(tx.send(m));
    if let Err(e) = std::fs::create_dir_all(&req.downloads) {
        for item in &req.items {
            send(Msg::Failed { key: item.key.clone(), reason: format!("{}: {e}", req.downloads.display()) });
        }
        return;
    }
    let client = Client::new(req.openalex_key.clone());
    let mut browser = Vec::new();
    for (n, item) in req.items.iter().enumerate() {
        send(Msg::Progress(format!("fetch {}/{}: {}…", n + 1, req.items.len(), item.key)));
        let Some(doi) = &item.doi else {
            let urls = item.openalex.as_deref().map(|id| client.pdf_urls_by_id(id).unwrap_or_default()).unwrap_or_default();
            match urls.iter().find_map(|u| yeda_net::fulltext::download(u, None).ok()) {
                Some(pdf) => {
                    let file = req.downloads.join(&item.file);
                    match std::fs::write(&file, pdf) {
                        Ok(()) => send(Msg::Saved { key: item.key.clone(), file, verify: false }),
                        Err(e) => send(Msg::Failed { key: item.key.clone(), reason: format!("{}: {e}", file.display()) }),
                    }
                }
                None => send(Msg::Failed { key: item.key.clone(), reason: "no DOI and no open-access copy; attach it with :attach".into() }),
            }
            continue;
        };
        // Open-access copies first, then the publisher directly, which works on campus by IP.
        let open = client.pdf_urls(doi).unwrap_or_default();
        let urls = open.into_iter().chain(fulltext::publisher_urls(doi, &req.settings));
        match urls.into_iter().find_map(|u| yeda_net::fulltext::download(&u, None).ok()) {
            Some(pdf) => {
                let file = req.downloads.join(&item.file);
                match std::fs::write(&file, pdf) {
                    Ok(()) => send(Msg::Saved { key: item.key.clone(), file, verify: false }),
                    Err(e) => send(Msg::Failed { key: item.key.clone(), reason: format!("{}: {e}", file.display()) }),
                }
                std::thread::sleep(PAUSE);
            }
            None => browser.push(item),
        }
    }
    if browser.is_empty() {
        return;
    }
    send(Msg::Progress(format!("fetch: {} papers need the browser window…", browser.len())));
    let mut pending: HashSet<String> = browser.iter().map(|i| i.key.clone()).collect();
    if let Err(e) = in_browser(&req, &browser, &mut pending, &send) {
        for key in &pending {
            send(Msg::Failed { key: key.clone(), reason: format!("fetch window: {e:#}") });
        }
        return;
    }
    for key in pending {
        send(Msg::Failed { key, reason: "fetch window closed".into() });
    }
}

/// Runs `yeda fetch` with the papers and relays what it reports.
fn in_browser(req: &Request, items: &[&Item], pending: &mut HashSet<String>, send: &dyn Fn(Msg)) -> anyhow::Result<()> {
    let papers: Vec<_> = items
        .iter()
        .map(|i| serde_json::json!({ "key": i.key, "doi": i.doi, "file": i.file }))
        .collect();
    let job = serde_json::json!({ "downloads": req.downloads, "settings": req.settings, "papers": papers });
    let mut child = Command::new(std::env::current_exe()?)
        .arg("fetch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child.stdin.take().unwrap().write_all(job.to_string().as_bytes())?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let v: serde_json::Value = serde_json::from_str(&line?)?;
        let key = v["key"].as_str().unwrap_or_default().to_string();
        if let Some(page) = v["page"].as_str() {
            send(Msg::Progress(format!("fetch window: {key}: {page}")));
            continue;
        }
        pending.remove(&key);
        match (v["file"].as_str(), v["error"].as_str()) {
            (Some(file), _) => send(Msg::Saved { key, file: file.into(), verify: true }),
            (_, error) => send(Msg::Failed { key, reason: error.unwrap_or("no reason given").into() }),
        }
    }
    child.wait()?;
    Ok(())
}
