mod app;
mod citations;
mod cluster;
mod complete;
mod discover;
mod fetch;
mod keys;
mod log;
mod poppler;
mod setup;
mod sync;
mod table;
mod ui;
mod undo;
mod wizard;

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
       yeda import   (a form that moves a Zotero library, PDFs included, to a new yeda library)
       yeda init     (the same form for a new, empty library)
       yeda read <file.pdf> [--annotations <dir>] [--page N]
       yeda import-zotero <zotero data dir> [library dir] [--pdfs-to <location>]
       yeda import-table <table name> <file.csv> [library dir]
       yeda copy-pdfs <from location> <to location> [library dir]";

/// `yeda import` and `yeda init`: asks in a form, then (importing) imports from Zotero, adds the
/// PDF location, copies every imported PDF there, and makes the library the default if asked.
fn wizard(import: bool) -> Result<()> {
    let config = ensure_config()?;
    let mut term = ratatui::init();
    execute!(std::io::stdout(), event::EnableBracketedPaste)?;
    let answers = wizard::run(&mut term, import, "~/yeda");
    let _ = execute!(std::io::stdout(), event::DisableBracketedPaste);
    ratatui::restore();
    let Some(a) = answers? else { return Ok(()) };
    let dir = &a.library;
    fs::create_dir_all(dir)?;
    // An empty yeda.toml is what makes a folder a library.
    if !dir.join(library::CONFIG).exists() {
        yeda_fs::write_atomic(&dir.join(library::CONFIG), b"")?;
    }
    sync::write_new(&dir.join(".gitignore"), sync::GITIGNORE)?;
    sync::write_new(&dir.join(".stignore"), sync::STIGNORE)?;
    if import {
        println!("{}", import_zotero(&a.zotero, dir)?);
    }
    if a.dest != wizard::Dest::Nowhere {
        let mut lib = load_library(dir)?;
        let location: toml::Value = match &a.dav {
            Some((dav, _)) => toml::Table::from_iter([("url".to_string(), dav.url.clone().into()), ("user".to_string(), dav.user.clone().into())]).into(),
            None => {
                fs::create_dir_all(&a.folder)?;
                sync::write_new(&a.folder.join(".stignore"), "*.tmp\n")?;
                a.folder.to_string_lossy().into_owned().into()
            }
        };
        let effect = lib.update_config(|t| {
            library::config_section(t, "locations")?.insert(a.name.clone(), location);
            Ok(())
        })?;
        yeda_fs::apply(dir, vec![effect])?;
        if let Some((dav, password)) = &a.dav {
            remember_password(dav, password);
            if let Err(e) = yeda_secrets::set(&yeda_secrets::webdav_account(&dav.user, &dav.url), password) {
                eprintln!("the WebDAV password wasn't saved to the secret store ({e:#}); yeda will ask for it next time");
            }
        }
        let lib = load_library(dir)?;
        let sources: std::collections::BTreeSet<String> = lib.refs.iter().flat_map(|r| &r.pdfs).map(|p| p.location.clone()).filter(|l| *l != a.name).collect();
        for from in sources {
            copy_pdfs(&from, &a.name, dir)?;
        }
    }
    if a.make_default {
        set_default_library(config.as_deref(), dir)?;
    }
    println!("done; open it with: yeda{}", if a.make_default { String::new() } else { format!(" {}", dir.display()) });
    Ok(())
}

/// `~/.config/yeda/config.toml`, written from the template when it doesn't exist.
fn ensure_config() -> Result<Option<PathBuf>> {
    let config = dirs::config_dir().map(|d| d.join("yeda/config.toml"));
    if let Some(path) = config.as_deref().filter(|p| !p.exists()) {
        fs::create_dir_all(path.parent().unwrap())?;
        yeda_fs::write_atomic(path, CONFIG_TEMPLATE.as_bytes())?;
    }
    Ok(config)
}

/// Sets `library =` in config.toml, replacing the line that sets or shows it, so the comments stay.
fn set_default_library(config: Option<&Path>, dir: &Path) -> Result<()> {
    let Some(config) = config else { return Ok(()) };
    let home = dirs::home_dir();
    let shown = match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    };
    let line = format!("library = {}", toml::Value::String(shown));
    let text = fs::read_to_string(config).unwrap_or_default();
    let at = text.lines().position(|l| l.trim_start_matches(['#', ' ']).starts_with("library ="));
    let text = match at {
        Some(i) => text.lines().enumerate().map(|(n, l)| if n == i { line.as_str() } else { l }).collect::<Vec<_>>().join("\n") + "\n",
        None => format!("{line}\n{text}"),
    };
    yeda_fs::write_atomic(config, text.as_bytes())?;
    Ok(())
}

/// Written to `~/.config/yeda/config.toml` when there is none.
const CONFIG_TEMPLATE: &str = r#"# Settings for this device. Everything else goes in the library's yeda.toml.

# The library yeda opens when given no folder and not started inside one.
# library = "~/papers"

# The PDF reader: "default" is yeda's own. Another command is run as <command> <pdf>
# --annotations <dir>, and yeda sees only the annotations it saves in the PDF (x extracts them).
# reader = "default"

# This device's folders for the library's PDF locations.
# [locations]
# zotero = "~/Zotero/storage"

# Key bindings, per context (normal, library, graph, cluster, table, annotations); "" unbinds a key.
# [keys.normal]
# "ctrl-d" = "half-down"
"#;

/// `arg` if given; else the current folder if it is a library; else `library` from config.toml; else the current folder.
fn library_dir(arg: Option<&String>) -> Result<PathBuf> {
    if let Some(a) = arg {
        return Ok(a.into());
    }
    let here = PathBuf::from(".");
    if here.join("yeda.toml").exists() || here.join("refs").is_dir() {
        return Ok(here);
    }
    #[derive(serde::Deserialize)]
    struct Machine {
        library: Option<String>,
    }
    let config = dirs::config_dir().map(|d| d.join("yeda/config.toml"));
    let text = config.as_deref().map(yeda_fs::read_optional).transpose()?.flatten();
    let machine: Option<Machine> = text.map(|t| toml::from_str(&t)).transpose().context("config.toml")?;
    Ok(match machine.and_then(|m| m.library) {
        Some(l) => match (l.strip_prefix("~/"), dirs::home_dir()) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => l.into(),
        },
        None => here,
    })
}

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
        eprint!("copying Zotero's database…");
        for name in ["zotero.sqlite", "zotero.sqlite-wal"] {
            let src = zotero.join(name);
            if src.exists() {
                fs::copy(&src, tmp.join(name)).with_context(|| src.display().to_string())?;
            }
        }
        let db = rusqlite::Connection::open(tmp.join("zotero.sqlite"))?;
        eprint!("\rreading items and annotations…{:20}", "");
        let mut images = 0;
        let mut image = |src: &zotero::ImageSource| {
            images += 1;
            eprint!("\rimage annotations: {images} (from Zotero's cache or rendered from the PDF){:10}", "");
            fs::read(&src.cached).ok().or_else(|| {
                let (page, rect) = src.area?;
                poppler::render_area(src.pdf?, page, rect).ok()
            })
        };
        let linked = zotero_pref("baseAttachmentPath").map(PathBuf::from);
        let (report, effects) = zotero::import(&db, zotero, linked.as_deref(), load_library(dir)?, dirs::home_dir().as_deref(), &mut image)?;
        eprint!("\rwriting {} files…{:60}", effects.len(), "");
        yeda_fs::apply(dir, effects)?;
        eprint!("\r{:80}\r", "");
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
    if let Some(command @ ("import" | "init")) = args.first().map(String::as_str) {
        return wizard(command == "import");
    }
    if args.first().map(String::as_str) == Some("import-zotero") {
        let mut args = args.clone();
        let pdfs_to = args.iter().position(|a| a == "--pdfs-to").map(|i| {
            let to = args.get(i + 1).cloned();
            args.drain(i..(i + 2).min(args.len()));
            to
        });
        let zotero = args.get(1).context(USAGE)?;
        let lib = &library_dir(args.get(2))?;
        println!("{}", import_zotero(Path::new(zotero), lib)?);
        if let Some(to) = pdfs_to {
            copy_pdfs(ZOTERO_LOCATION, &to.context(USAGE)?, lib)?;
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("copy-pdfs") {
        let (Some(from), Some(to)) = (args.get(1), args.get(2)) else { anyhow::bail!(USAGE) };
        return copy_pdfs(from, to, &library_dir(args.get(3))?);
    }
    if args.first().map(String::as_str) == Some("import-table") {
        let (Some(name), Some(csv)) = (args.get(1), args.get(2)) else { anyhow::bail!(USAGE) };
        let dir = &library_dir(args.get(3))?;
        let mut lib = load_library(dir)?;
        let csv = fs::read_to_string(csv).with_context(|| csv.clone())?;
        let (report, effects) = tables::import_csv(&mut lib, name, &csv)?;
        yeda_fs::apply(dir, effects)?;
        println!("{report}");
        return Ok(());
    }
    let config = ensure_config()?;
    let dir = library_dir(args.first())?;
    let dir = dir.canonicalize().unwrap_or(dir);
    let lib = load_library(&dir)?;
    let user = config.as_deref().map(yeda_fs::read_optional).transpose()?.flatten();
    let keymap = keys::Keymap::load(user.as_deref())?;
    let mut term = ratatui::init();
    execute!(std::io::stdout(), event::EnableBracketedPaste, event::EnableMouseCapture)?;
    let picker = detect_picker();
    let mut app = App::new(dir, lib, picker, keymap);
    let res = run(&mut term, &mut app);
    let _ = execute!(std::io::stdout(), event::DisableBracketedPaste, event::DisableMouseCapture);
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

/// A string setting `extensions.zotero.<name>` from Zotero's preferences.
fn zotero_pref(name: &str) -> Option<String> {
    let base = dirs::home_dir()?.join(".zotero/zotero");
    let profiles = fs::read_to_string(base.join("profiles.ini")).ok()?;
    let profile = profiles.lines().find_map(|l| l.strip_prefix("Path="))?;
    let prefs = fs::read_to_string(base.join(profile).join("prefs.js")).ok()?;
    let line = prefs.lines().find(|l| l.contains(&format!("\"extensions.zotero.{name}\"")))?;
    line.rsplit(", \"").next()?.split('"').next().map(String::from)
}

/// Zotero's file-sync WebDAV folder, from its preferences, if it syncs files over WebDAV.
fn zotero_webdav() -> Option<library::Dav> {
    let pref = |name: &str| zotero_pref(&format!("sync.storage.{name}"));
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
    if let Some(p) = KNOWN_PASSWORDS.lock().unwrap().get(&account) {
        return Ok(p.clone());
    }
    if let Ok(Some(p)) = yeda_secrets::get(&account) {
        return Ok(p);
    }
    let p = ask_password(&format!("{}@{}", dav.user, dav.url))?;
    remember_password(dav, &p);
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

/// WebDAV passwords entered or tested in this run, by secret-store account, so a run asks once.
static KNOWN_PASSWORDS: std::sync::Mutex<std::collections::BTreeMap<String, String>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn remember_password(dav: &library::Dav, password: &str) {
    KNOWN_PASSWORDS.lock().unwrap().insert(yeda_secrets::webdav_account(&dav.user, &dav.url), password.into());
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
    let todo: Vec<(usize, usize)> =
        lib.refs.iter().enumerate().flat_map(|(i, r)| r.pdfs.iter().enumerate().filter(|(_, p)| p.location == from).map(move |(j, _)| (i, j))).collect();
    let mut failed = Vec::new();
    for (n, &(i, j)) in todo.iter().enumerate() {
        let key = lib.refs[i].key.clone();
        eprint!("\r{}/{} {key:<40}", n + 1, todo.len());
        let result = (|| -> Result<library::Ref> {
            let r = &lib.refs[i];
            let pdf = &r.pdfs[j];
            let bytes = match (lib.remote_pdf(pdf), &from_password) {
                (Some(remote), Some(p)) => yeda_net::webdav::download(remote.dav, p, &remote.path)?.0,
                _ => {
                    let path = lib.pdf_path(pdf)?;
                    match (fs::read(&path), &zotero_dav) {
                        (Ok(bytes), _) => bytes,
                        (Err(e), Some(z)) if e.kind() == std::io::ErrorKind::NotFound => {
                            let password = match &zotero_password {
                                Some(p) => p.clone(),
                                None => zotero_password.insert(dav_password(z)?).clone(),
                            };
                            from_zotero_webdav(z, &password, &pdf.path)?
                        }
                        (Err(e), _) => return Err(anyhow::Error::from(e).context(path.display().to_string())),
                    }
                }
            };
            // The copy keeps its id, and so its annotations.
            let copy = library::Pdf { id: pdf.id.clone(), ..lib.attach(r, to, &pdf.path)? };
            let mut r = r.clone();
            r.pdfs[j] = copy.clone();
            let dest = lib.pdf_path(&copy)?;
            yeda_fs::write_atomic(&dest, &bytes)?;
            if let (Some(remote), Some(p)) = (lib.remote_pdf(&copy), &to_password) {
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
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
        }
        app.poll_worker();
        app.poll_graph();
        app.poll_fetch();
        app.poll_setup();
        app.poll_discover();
        app.log.write(&app.status);
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
        if let Some(url) = app.git_init.take() {
            suspend(term, || {
                let result = sync::init(&app.dir, url.as_deref());
                if let Err(e) = &result {
                    println!("{e:#}\npress Enter to return");
                    let _ = std::io::stdin().read_line(&mut String::new());
                }
                app.status = result.unwrap_or_else(|e| format!("git-init: {e:#}"));
            })?;
        }
        if let Some((path, target)) = app.edit_text.take() {
            let mut result = None;
            suspend(term, || result = Some(editor().arg(&path).status()))?;
            match result.unwrap() {
                Ok(_) => app.text_edited(&path, target),
                Err(e) => app.status = format!("editor: {e}"),
            }
        }
        if let Some((path, line)) = app.edit.take() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let mut result = None;
            suspend(term, || result = Some(editor().args(line.map(|n| format!("+{n}"))).arg(&path).status()))?;
            match result.unwrap() {
                Ok(_) => app.reload(),
                Err(e) => app.status = format!("editor: {e}"),
            }
        }
    }
    Ok(())
}

/// `$EDITOR`, which may carry arguments (`code -w`), else vi.
fn editor() -> Command {
    let editor = std::env::var("EDITOR").unwrap_or_default();
    let mut words = editor.split_whitespace();
    let mut command = Command::new(words.next().unwrap_or("vi"));
    command.args(words);
    command
}

/// Hands the terminal to `f`, then takes it back.
fn suspend(term: &mut DefaultTerminal, f: impl FnOnce()) -> Result<()> {
    execute!(std::io::stdout(), event::DisableBracketedPaste, event::DisableMouseCapture)?;
    ratatui::restore();
    f();
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen, event::EnableBracketedPaste, event::EnableMouseCapture)?;
    term.clear()?;
    Ok(())
}
