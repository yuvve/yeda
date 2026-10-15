mod annotations;
mod app;
mod library;
mod llm;
mod search;
mod tables;
mod ui;
mod wand;
mod zotero;

use anyhow::{Context, Result};
use app::App;
use library::Library;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use std::io::Write;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::{execute, terminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: yeda [library dir]
       yeda import-zotero <zotero data dir> [library dir]
       yeda import-table <table name> <file.csv> [library dir]";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("import-zotero") {
        let zotero = args.get(1).context(USAGE)?;
        let lib = args.get(2).map_or(".", String::as_str);
        println!("{}", zotero::import(Path::new(zotero), Path::new(lib))?);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("import-table") {
        let (Some(name), Some(csv)) = (args.get(1), args.get(2)) else { anyhow::bail!(USAGE) };
        let mut lib = Library::load(Path::new(args.get(3).map_or(".", String::as_str)))?;
        println!("{}", tables::import_csv(&mut lib, name, Path::new(csv))?);
        return Ok(());
    }
    let dir = args.first().map(PathBuf::from).unwrap_or_else(|| ".".into());
    let dir = dir.canonicalize().unwrap_or(dir);
    let lib = Library::load(&dir)?;
    let mut term = ratatui::init();
    let picker = detect_picker();
    let mut app = App::new(lib, picker);
    let res = run(&mut term, &mut app);
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
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            app.on_key(k);
        }
        app.poll_worker();
        if let Some(job) = app.job.take() {
            term.draw(|f| ui::draw(f, app))?;
            app.run_job(job);
        }
        if let Some(path) = app.edit.take() {
            ratatui::restore();
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let result = Command::new(editor).arg(&path).status();
            terminal::enable_raw_mode()?;
            execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
            term.clear()?;
            match result {
                Ok(_) => app.reload(),
                Err(e) => app.status = format!("editor: {e}"),
            }
        }
    }
    Ok(())
}
