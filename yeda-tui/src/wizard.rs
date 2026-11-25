use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use std::path::PathBuf;
use yeda_core::library::Dav;

/// Where the PDFs go; `Nowhere` keeps imported ones in Zotero's storage, or sets no location yet.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Dest {
    Folder,
    Webdav,
    Dropbox,
    Nowhere,
}

const IMPORT_DESTS: [(Dest, &str); 4] =
    [(Dest::Folder, "copy to a folder"), (Dest::Webdav, "upload to WebDAV"), (Dest::Dropbox, "upload to Dropbox"), (Dest::Nowhere, "leave in Zotero's storage")];
const INIT_DESTS: [(Dest, &str); 4] = [(Dest::Folder, "a folder"), (Dest::Webdav, "a WebDAV folder"), (Dest::Dropbox, "a Dropbox folder"), (Dest::Nowhere, "none yet")];

/// What `yeda import` or `yeda init` was told.
pub struct Answers {
    pub zotero: PathBuf,
    pub library: PathBuf,
    pub dest: Dest,
    pub folder: PathBuf,
    /// The WebDAV folder and its password, tested.
    pub dav: Option<(Dav, String)>,
    /// The `[locations]` name the PDFs go under.
    pub name: String,
    pub make_default: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum F {
    Zotero,
    Library,
    Dest,
    Folder,
    Url,
    User,
    Password,
    Name,
    Default,
}

const FIELDS: [(F, &str, &str); 9] = [
    (F::Zotero, "Zotero", "Zotero's data folder, the one holding zotero.sqlite; Zotero is only read"),
    (F::Library, "Library", "the new library's folder: created if missing; an existing yeda library gets what it lacks"),
    (F::Dest, "PDFs", "←/→ picks. Missing local files come from Zotero's WebDAV; linked files are copied too"),
    (F::Folder, "Folder", "where the PDFs are copied to, as <citekey>/<file>; created if missing"),
    (F::Url, "WebDAV URL", "the folder PDFs go in, e.g. https://dav.example.com/yeda/; created if missing"),
    (F::User, "User", ""),
    (F::Password, "Password", "kept in the secret store"),
    (F::Name, "Location", "the name references use for where their PDF is"),
    (F::Default, "Default", "←/→: open this library when yeda is given no folder (library = in config.toml)"),
];

struct Form {
    /// `yeda import`: from Zotero; else `yeda init`.
    import: bool,
    values: Vec<String>,
    dest: usize,
    make_default: bool,
    sel: usize,
    error: String,
}

impl Form {
    fn get(&self, f: F) -> &str {
        self.values[f as usize].trim()
    }

    fn dests(&self) -> &'static [(Dest, &'static str); 4] {
        if self.import { &IMPORT_DESTS } else { &INIT_DESTS }
    }

    fn visible(&self) -> Vec<usize> {
        let dest = self.dests()[self.dest].0;
        (0..FIELDS.len())
            .filter(|&i| match FIELDS[i].0 {
                F::Zotero => self.import,
                F::Folder => matches!(dest, Dest::Folder | Dest::Dropbox),
                F::Url | F::User | F::Password => dest == Dest::Webdav,
                F::Name => dest != Dest::Nowhere,
                _ => true,
            })
            .collect()
    }

    fn hint(&self) -> &'static str {
        match (FIELDS[self.sel].0, self.import) {
            (F::Library, false) => "the new library's folder: created if missing",
            (F::Dest, false) => "←/→ picks where PDFs you add or fetch are kept; more locations can be added later",
            (F::Folder, false) if self.dests()[self.dest].0 == Dest::Dropbox => "inside yeda's Dropbox app folder, as <citekey>/<file>; empty is its root",
            (F::Folder, false) => "where PDFs are kept, as <citekey>/<file>; created if missing",
            _ => FIELDS[self.sel].2,
        }
    }

    fn step(&mut self, by: isize) {
        let vis = self.visible();
        let at = vis.iter().position(|&i| i == self.sel).unwrap_or(0) as isize;
        self.sel = vis[(at + by).rem_euclid(vis.len() as isize) as usize];
    }

    fn choose(&mut self, by: isize) {
        match FIELDS[self.sel].0 {
            F::Dest => {
                let before = self.dests()[self.dest].0;
                self.dest = (self.dest as isize + by).rem_euclid(4) as usize;
                let after = self.dests()[self.dest].0;
                let name = &mut self.values[F::Name as usize];
                if *name == default_name(before) {
                    *name = default_name(after).into();
                }
                let folder = &mut self.values[F::Folder as usize];
                if *folder == default_folder(before) {
                    *folder = default_folder(after).into();
                }
            }
            F::Default => self.make_default = !self.make_default,
            _ => {}
        }
    }

    /// Checks the answers; a WebDAV folder is logged into, and created if missing.
    fn answers(&self) -> Result<Answers, String> {
        let zotero = expand(self.get(F::Zotero));
        if self.import && !zotero.join("zotero.sqlite").exists() {
            return Err(format!("{} has no zotero.sqlite", zotero.display()));
        }
        let library = expand(self.get(F::Library));
        let is_library = library.join("yeda.toml").exists() || library.join("refs").is_dir();
        if library.as_os_str().is_empty() || (!is_library && std::fs::read_dir(&library).is_ok_and(|mut d| d.next().is_some())) {
            return Err(format!("{} isn't empty and isn't a yeda library", library.display()));
        }
        let dest = self.dests()[self.dest].0;
        let name = self.get(F::Name).to_string();
        if dest != Dest::Nowhere && (name.is_empty() || ["zotero", "home", "files"].contains(&name.as_str())) {
            return Err("name the location (zotero, home and files are taken by the import)".into());
        }
        // A Dropbox folder is a path inside the app folder, not on this machine.
        let folder = if dest == Dest::Dropbox { PathBuf::from(self.get(F::Folder)) } else { expand(self.get(F::Folder)) };
        if (dest == Dest::Folder && folder.as_os_str().is_empty()) || (dest == Dest::Dropbox && !folder.components().all(|c| matches!(c, std::path::Component::Normal(_)))) {
            return Err("give the folder for the PDFs".into());
        }
        let dav = match dest {
            Dest::Webdav => {
                let (url, user, password) = (self.get(F::Url), self.get(F::User), self.values[F::Password as usize].clone());
                if user.is_empty() || password.is_empty() {
                    return Err("give the WebDAV URL (https://…), user and password".into());
                }
                yeda_core::require_https(url).map_err(|e| format!("{e:#}"))?;
                let dav = Dav { url: url.into(), user: user.into(), alias: None };
                yeda_net::webdav::login(&dav, &password).map_err(|e| format!("{e:#}"))?;
                Some((dav, password))
            }
            _ => None,
        };
        Ok(Answers { zotero, library, dest, folder, dav, name, make_default: self.make_default })
    }
}

fn default_name(dest: Dest) -> &'static str {
    match dest {
        Dest::Webdav => "dav",
        Dest::Dropbox => "dropbox",
        _ => "pdfs",
    }
}

/// Dropbox folders sit inside the app folder, so the default is not a path under `~`.
fn default_folder(dest: Dest) -> &'static str {
    match dest {
        Dest::Dropbox => "pdfs",
        _ => "~/yeda-pdfs",
    }
}

/// `~/x` under the home folder.
pub fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Asks for everything `yeda import` (or with `import` off, `yeda init`) needs; `None` when quit with Esc.
pub fn run(term: &mut DefaultTerminal, import: bool, library: &str) -> Result<Option<Answers>> {
    let mut values = vec![String::new(); FIELDS.len()];
    values[F::Zotero as usize] = "~/Zotero".into();
    values[F::Library as usize] = library.into();
    values[F::Folder as usize] = "~/yeda-pdfs".into();
    values[F::Name as usize] = default_name(Dest::Folder).into();
    let sel = if import { F::Zotero } else { F::Library } as usize;
    let mut form = Form { import, values, dest: 0, make_default: true, sel, error: String::new() };
    loop {
        term.draw(|f| draw(f, &form))?;
        let text = match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                form.error.clear();
                match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if ctrl => return Ok(None),
                    KeyCode::Down | KeyCode::Tab => form.step(1),
                    KeyCode::Up | KeyCode::BackTab => form.step(-1),
                    KeyCode::Right => form.choose(1),
                    KeyCode::Left => form.choose(-1),
                    KeyCode::Char('u') if ctrl => form.values[form.sel].clear(),
                    KeyCode::Backspace => drop(form.values[form.sel].pop()),
                    KeyCode::Char(c) if !ctrl => {
                        form.values[form.sel].push(c);
                    }
                    KeyCode::Enter => {
                        form.error = "checking…".into();
                        term.draw(|f| draw(f, &form))?;
                        match form.answers() {
                            Ok(a) => return Ok(Some(a)),
                            Err(e) => form.error = e,
                        }
                    }
                    _ => {}
                }
                continue;
            }
            Event::Paste(text) => text,
            _ => continue,
        };
        if !matches!(FIELDS[form.sel].0, F::Dest | F::Default) {
            form.values[form.sel].push_str(text.trim());
        }
    }
}

fn draw(f: &mut ratatui::Frame, form: &Form) {
    let mut lines = vec![Line::raw("")];
    for i in form.visible() {
        let (field, label, _) = FIELDS[i];
        let value = match field {
            F::Dest => format!("‹ {} ›", form.dests()[form.dest].1),
            F::Default => format!("‹ {} ›", if form.make_default { "yes" } else { "no" }),
            F::Password => "•".repeat(form.values[i].chars().count()),
            _ => form.values[i].clone(),
        };
        let cursor = if i == form.sel && !matches!(field, F::Dest | F::Default) { "▏" } else { "" };
        let line = Line::from(vec![Span::raw(format!("{label:>11}  ")).bold(), Span::raw(format!("{value}{cursor}"))]);
        lines.push(if i == form.sel { line.reversed() } else { line });
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(format!("{:>11}  {}", "", form.hint())).dim());
    if !form.error.is_empty() {
        lines.push(Line::raw(format!("{:>11}  {}", "", form.error)).fg(Color::Red));
    }
    let width = 100.min(f.area().width);
    let height = (lines.len() as u16 + 2).min(f.area().height);
    let [area] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(f.area());
    let [area] = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center).areas(area);
    f.render_widget(Clear, area);
    let title = if form.import { " Import from Zotero · Tab/↓ next · Enter start · Esc quit " } else { " New library · Tab/↓ next · Enter create · Esc quit " };
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title).border_style(Color::Cyan)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        let mut values = vec![String::new(); FIELDS.len()];
        values[F::Library as usize] = std::env::temp_dir().join(format!("yeda-wizard-{}", std::process::id())).to_string_lossy().into();
        values[F::Folder as usize] = default_folder(Dest::Folder).into();
        values[F::Name as usize] = default_name(Dest::Folder).into();
        Form { import: false, values, dest: 0, make_default: true, sel: F::Dest as usize, error: String::new() }
    }

    #[test]
    fn choosing_dropbox_takes_the_paths_back_to_its_app_folder() {
        let mut f = form();
        f.choose(1);
        assert_eq!(f.dests()[f.dest].0, Dest::Webdav);
        f.choose(1);
        assert_eq!(f.dests()[f.dest].0, Dest::Dropbox);
        assert_eq!((f.get(F::Name), f.get(F::Folder)), ("dropbox", "pdfs"));
        // The Dropbox folder is checked for escapes, not for a `~/` path.
        f.values[F::Folder as usize] = "../etc".into();
        assert!(f.answers().is_err());
        f.values[F::Folder as usize] = String::new();
        assert!(f.answers().unwrap().folder.as_os_str().is_empty());
        f.choose(1);
        assert_eq!(f.dests()[f.dest].0, Dest::Nowhere);
    }
}
