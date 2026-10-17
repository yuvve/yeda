mod app;
mod citations;
mod poppler;
mod sync;
mod ui;

use anyhow::{Context, Result};
use app::App;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use std::io::Write;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::{execute, terminal};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use yeda_core::library::Library;
use yeda_core::{tables, zotero};

const USAGE: &str = "usage: yeda [library dir]
       yeda read <file.pdf> [--annotations <dir>] [--page N]
       yeda import-zotero <zotero data dir> [library dir]
       yeda import-table <table name> <file.csv> [library dir]";

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
    if args.first().map(String::as_str) == Some("import-zotero") {
        let zotero = args.get(1).context(USAGE)?;
        let lib = args.get(2).map_or(".", String::as_str);
        println!("{}", import_zotero(Path::new(zotero), Path::new(lib))?);
        return Ok(());
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
        if let Some(path) = app.edit.take() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let mut result = None;
            suspend(term, || result = Some(Command::new(editor).arg(&path).status()))?;
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
