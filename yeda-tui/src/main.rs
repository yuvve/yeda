mod app;
mod citations;
mod fetch;
mod poppler;
mod sync;
mod ui;

use anyhow::{Context, Result};
use app::App;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use std::io::{IsTerminal, Write};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::{execute, terminal};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use yeda_core::library::{self, Library};
use yeda_core::{tables, zotero};

const USAGE: &str = "usage: yeda [library dir]
       yeda read <file.pdf> [--annotations <dir>] [--page N]
       yeda import-zotero <zotero data dir> [library dir] [--pdfs-to <location>]
       yeda import-table <table name> <file.csv> [library dir]
       yeda copy-pdfs <from location> <to location> [library dir]";

pub fn load_library(dir: &Path) -> Result<Library> {
    let machine = dirs::config_dir().map(|d| d.join("yeda/config.toml"));
    let mut lib = yeda_fs::load_library(dir, machine.as_deref(), dirs::home_dir().as_deref())?;
    lib.cache = dirs::cache_dir().map(|d| d.join("yeda"));
    Ok(lib)
}

fn import_zotero(zotero: &Path, dir: &Path) -> Result<String> {
    // Zotero holds an exclusive lock on its database while running, so read a copy.
    let tmp = std::env::temp_dir().join(format!("yeda-zotero-{}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let result = (|| {
        for name in ["zotero.sqlite", "zotero.sqlite-wal"] {
            let src = zotero.join(name);
            if src.exists() {
                fs::copy(&src, tmp.join(name)).with_context(|| src.display().to_string())?;
            }
        }
        let db = rusqlite::Connection::open(tmp.join("zotero.sqlite"))?;
        let mut image = |src: &zotero::ImageSource| {
            fs::read(&src.cached).ok().or_else(|| {
                let (page, rect) = src.area?;
                poppler::render_area(src.pdf?, page, rect).ok()
            })
        };
        let (report, effects) = zotero::import(&db, zotero, load_library(dir)?, dirs::home_dir().as_deref(), &mut image)?;
        yeda_fs::apply(dir, effects)?;
        Ok(report)
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("read") {
        yeda_reader_lib::run_with(args.into_iter().skip(1));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("fetch") {
        let job: yeda_reader_lib::fetch::Job = serde_json::from_reader(std::io::stdin()).context("fetch: a job as JSON on stdin")?;
        yeda_reader_lib::fetch::run(job);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("import-zotero") {
        let mut args = args.clone();
        let pdfs_to = args.iter().position(|a| a == "--pdfs-to").map(|i| {
            let to = args.get(i + 1).cloned();
            args.drain(i..(i + 2).min(args.len()));
            to
        });
        let zotero = args.get(1).context(USAGE)?;
        let lib = Path::new(args.get(2).map_or(".", String::as_str));
        println!("{}", import_zotero(Path::new(zotero), lib)?);
        if let Some(to) = pdfs_to {
            copy_pdfs(ZOTERO_LOCATION, &to.context(USAGE)?, lib)?;
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("copy-pdfs") {
        let (Some(from), Some(to)) = (args.get(1), args.get(2)) else { anyhow::bail!(USAGE) };
        return copy_pdfs(from, to, Path::new(args.get(3).map_or(".", String::as_str)));
    }
    if args.first().map(String::as_str) == Some("import-table") {
        let (Some(name), Some(csv)) = (args.get(1), args.get(2)) else { anyhow::bail!(USAGE) };
        let dir = Path::new(args.get(3).map_or(".", String::as_str));
        let mut lib = load_library(dir)?;
        let csv = fs::read_to_string(csv).with_context(|| csv.clone())?;
        let (report, effects) = tables::import_csv(&mut lib, name, &csv)?;
        yeda_fs::apply(dir, effects)?;
        println!("{report}");
        return Ok(());
    }
    let dir = args.first().map(PathBuf::from).unwrap_or_else(|| ".".into());
    let dir = dir.canonicalize().unwrap_or(dir);
    let lib = load_library(&dir)?;
    let mut term = ratatui::init();
    execute!(std::io::stdout(), event::EnableBracketedPaste)?;
    let picker = detect_picker();
    let mut app = App::new(dir, lib, picker);
    let res = run(&mut term, &mut app);
    let _ = execute!(std::io::stdout(), event::DisableBracketedPaste);
    ratatui::restore();
    res
}

/// Reads a password from the terminal without echoing it.
fn ask_password(label: &str) -> Result<String> {
    use event::{KeyCode, KeyModifiers};
    eprint!("password for {label}: ");
    std::io::stderr().flush()?;
    terminal::enable_raw_mode()?;
    let mut password = String::new();
    let result = loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break Err(anyhow::anyhow!("cancelled")),
                KeyCode::Backspace => drop(password.pop()),
                KeyCode::Char(c) => password.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    terminal::disable_raw_mode()?;
    eprintln!();
    result.map(|()| password)
}

/// The location `import-zotero` creates for Zotero's storage folder.
const ZOTERO_LOCATION: &str = "zotero";

/// Zotero's file-sync WebDAV folder, from its preferences, if it syncs files over WebDAV.
fn zotero_webdav() -> Option<library::Dav> {
    let base = dirs::home_dir()?.join(".zotero/zotero");
    let profiles = fs::read_to_string(base.join("profiles.ini")).ok()?;
    let profile = profiles.lines().find_map(|l| l.strip_prefix("Path="))?;
    let prefs = fs::read_to_string(base.join(profile).join("prefs.js")).ok()?;
    let pref = |name: &str| {
        let line = prefs.lines().find(|l| l.contains(&format!("\"extensions.zotero.sync.storage.{name}\"")))?;
        line.rsplit(", \"").next()?.split('"').next().map(String::from)
    };
    if pref("protocol")? != "webdav" {
        return None;
    }
    // Zotero keeps its files in a `zotero` folder under the configured URL.
    let url = format!("{}://{}/zotero", pref("scheme").unwrap_or_else(|| "https".into()), pref("url")?.trim_end_matches('/'));
    Some(library::Dav { url, user: pref("username")? })
}

/// The WebDAV password for `dav`, from the secret store or asked for in the terminal.
fn dav_password(dav: &library::Dav) -> Result<String> {
    let account = yeda_secrets::webdav_account(&dav.user, &dav.url);
    if let Ok(Some(p)) = yeda_secrets::get(&account) {
        return Ok(p);
    }
    let p = ask_password(&format!("{}@{}", dav.user, dav.url))?;
    if std::io::stdin().is_terminal() {
        eprint!("save it in the secret store? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            yeda_secrets::set(&account, &p)?;
        }
    }
    Ok(p)
}

/// A file from Zotero's WebDAV, where attachment `KEY/file` is stored as `KEY.zip`. Only reads.
fn from_zotero_webdav(dav: &library::Dav, password: &str, path: &str) -> Result<Vec<u8>> {
    let (key, file) = path.split_once('/').context("not a Zotero storage path")?;
    let (zip, _) = yeda_net::webdav::download(dav, password, &format!("{key}.zip"))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("not a zip file")?;
    let index = archive.index_for_name(file).or((archive.len() == 1).then_some(0)).with_context(|| format!("{file} is not in {key}.zip"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_index(index)?, &mut bytes)?;
    Ok(bytes)
}

/// Copies every PDF at location `from` to location `to`, as `<citekey>/<file>`, and points the
/// references at the copies. The originals stay where they are. For Zotero's storage folder, a
/// file missing on disk is downloaded from Zotero's WebDAV, which is only read.
fn copy_pdfs(from: &str, to: &str, dir: &Path) -> Result<()> {
    let mut lib = load_library(dir)?;
    let zotero_dav = (from == ZOTERO_LOCATION).then(zotero_webdav).flatten();
    if let (Some(z), Some(target)) = (&zotero_dav, lib.remotes.get(to))
        && target.url.trim_end_matches('/').starts_with(z.url.trim_end_matches('/'))
    {
        anyhow::bail!("{to} is inside Zotero's WebDAV folder ({}); use a folder of its own", z.url);
    }
    let mut zotero_password: Option<String> = None;
    let password = |name: &str| lib.remotes.get(name).map(dav_password).transpose();
    let (from_password, to_password) = (password(from)?, password(to)?);
    let todo: Vec<usize> = (0..lib.refs.len()).filter(|&i| lib.refs[i].pdf.as_ref().is_some_and(|p| p.location == from)).collect();
    let mut failed = Vec::new();
    for (n, &i) in todo.iter().enumerate() {
        let key = lib.refs[i].key.clone();
        eprint!("\r{}/{} {key:<40}", n + 1, todo.len());
        let result = (|| -> Result<library::Ref> {
            let r = &lib.refs[i];
            let bytes = match (lib.remote_pdf(r), &from_password) {
                (Some(remote), Some(p)) => yeda_net::webdav::download(remote.dav, p, &remote.path)?.0,
                _ => {
                    let path = lib.pdf_path(r)?;
                    match (fs::read(&path), &zotero_dav) {
                        (Ok(bytes), _) => bytes,
                        (Err(e), Some(z)) if e.kind() == std::io::ErrorKind::NotFound => {
                            let password = match &zotero_password {
                                Some(p) => p.clone(),
                                None => zotero_password.insert(dav_password(z)?).clone(),
                            };
                            from_zotero_webdav(z, &password, &r.pdf.as_ref().unwrap().path)?
                        }
                        (Err(e), _) => return Err(anyhow::Error::from(e).context(path.display().to_string())),
                    }
                }
            };
            let old = r.pdf.as_ref().unwrap().path.clone();
            let mut r = r.clone();
            r.pdf = Some(lib.attach(&r, to, &old)?);
            let dest = lib.pdf_path(&r)?;
            yeda_fs::write_atomic(&dest, &bytes)?;
            if let (Some(remote), Some(p)) = (lib.remote_pdf(&r), &to_password) {
                yeda_net::webdav::upload(remote.dav, p, &remote.path, &bytes)?;
                yeda_fs::set_cached_version(&dest, yeda_net::webdav::current_version(remote.dav, p, &remote.path)?.as_deref())?;
            }
            yeda_fs::apply(dir, [lib.save(&r)?])?;
            Ok(r)
        })();
        match result {
            Ok(r) => lib.refs[i] = r,
            Err(e) if yeda_net::webdav::is_unauthorized(&e) => return Err(e),
            Err(e) => failed.push(format!("{key}: {e:#}")),
        }
    }
    eprintln!("\rcopied {} of {} PDFs from {from} to {to}{:30}", todo.len() - failed.len(), todo.len(), "");
    for f in &failed {
        eprintln!("  failed: {f}");
    }
    Ok(())
}

fn detect_picker() -> Picker {
    let options = QueryStdioOptions::default();
    let deadline = options.timeout;
    let started = Instant::now();
    let picker = Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks());
    // On timeout (e.g. tmux without passthrough) ratatui-image leaves a thread blocked on stdin
    // that would swallow the first keypress; a plain status request gives it a reply to exit on.
    if started.elapsed() >= deadline {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[5n").and_then(|_| out.flush());
    }
    picker
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        term.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                Event::Paste(text) => app.on_paste(&text),
                _ => {}
            }
        }
        app.poll_worker();
        app.poll_graph();
        app.poll_fetch();
        if let Some(job) = app.job.take() {
            term.draw(|f| ui::draw(f, app))?;
            app.run_job(job);
        }
        if std::mem::take(&mut app.sync) {
            suspend(term, || {
                let result = sync::run(&app.dir);
                if let Err(e) = &result {
                    println!("{e:#}\npress Enter to return");
                    let _ = std::io::stdin().read_line(&mut String::new());
                }
                app.status = result.unwrap_or_else(|e| format!("sync: {e:#}"));
            })?;
            app.reload();
        }
        if let Some((path, line)) = app.edit.take() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let mut result = None;
            suspend(term, || result = Some(Command::new(editor).args(line.map(|n| format!("+{n}"))).arg(&path).status()))?;
            match result.unwrap() {
                Ok(_) => app.reload(),
                Err(e) => app.status = format!("editor: {e}"),
            }
        }
    }
    Ok(())
}

/// Hands the terminal to `f`, then takes it back.
fn suspend(term: &mut DefaultTerminal, f: impl FnOnce()) -> Result<()> {
    execute!(std::io::stdout(), event::DisableBracketedPaste)?;
    ratatui::restore();
    f();
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen, event::EnableBracketedPaste)?;
    term.clear()?;
    Ok(())
}
