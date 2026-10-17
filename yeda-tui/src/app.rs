use crate::{citations, fetch, poppler};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::process::{Command, Stdio};
use yeda_core::Effect;
use yeda_core::annotations::{self, Annotation};
use yeda_core::ids;
use yeda_core::browse::{self, Filter, Sort, SortField};
use yeda_core::library::{self, Library, Ref, config_section};
use yeda_core::{cite, fulltext};
use yeda_core::graph::{self, Graph};
use yeda_core::search::{self, FILTER_FIELDS, Searcher};
use yeda_core::tables::{self, Row};
use yeda_net::llm::{self, Paper, Prompt as LlmPrompt};
use yeda_net::wand;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Normal,
    Search,
    SideSearch,
    Pick,
    Prompt(Prompt),
    ConfirmDelete,
    /// Asks whether to keep the secret just entered in the system secret store.
    ConfirmSaveSecret,
    Filter,
    Sort,
    Help(HelpPage),
}

#[derive(Clone, Copy, PartialEq)]
pub enum HelpPage {
    /// `HELP`, from `?` or `:keys`.
    Keys,
    /// `GUIDE`, from `:help`.
    Guide,
    /// `App::report`: the identifiers the last magic-wand batch could not add, with why.
    Failed,
}

pub const GUIDE: &str = include_str!("guide.txt");

/// Every key and command, as `:help` lists them.
pub const HELP: &[(&str, &str)] = &[
    ("j k Ctrl-d Ctrl-u gg G", "move"),
    ("h l Tab", "switch column"),
    ("c", "show only the current column; again to show all"),
    ("/", "search; in the library column, filter it (:help explains the syntax)"),
    ("f", "filter by field, with suggestions from the library"),
    ("s", "sort"),
    ("Esc", "clear the search"),
    ("a", "add by DOI, arXiv ID or ISBN; paste a list to add several"),
    ("o", "open the PDF in the reader (yeda read)"),
    ("e  n", "edit the reference file / its note in $EDITOR"),
    ("v  V", "mark or unmark one / mark a range until Esc; Esc again unmarks all"),
    ("t  m", "edit tags / move to a collection"),
    ("r  R", "next / previous reading status"),
    ("A  T", "show annotations / tables"),
    ("x", "extract the annotations embedded in the PDF"),
    ("C", "colour the selected tag (library column)"),
    ("dd", "delete the reference or saved search"),
    ("?", "this list"),
    ("q", "quit"),
    ("", ""),
    (":save <name>", "save the current search"),
    (":summarize[!] [table] [profile]", "fill the reference's table row with an LLM; ! overwrites"),
    (":summarize-all[!] [table] [profile]", "the same for every visible reference"),
    (":cancel", "stop summarizing after the current paper"),
    (":export <table> <file.csv>", "export a table"),
    (":attach <file.pdf> [location]", "attach a PDF, copying or uploading it to a location"),
    (":sync", "commit, pull and push the library with git"),
    (":graph[!] [selected|filtered|prefetch]", "citation graph from OpenAlex (:help explains it)"),
    (":graph key", "set an optional OpenAlex API key, kept in the secret store"),
    (":cite [format]", "copy the selected or marked references: bibtex, a style (ieee, apa, …) or a .csl file"),
    (":fetch [location]", "find and attach PDFs for the selected or marked references (:help explains it)"),
    (":keys", "this list"),
    (":help", "guide: search syntax (AND, OR, NOT, fields), filters, saved searches"),
    (":failed", "what the last add or fetch could not get, with the reasons"),
    (":q", "quit"),
];

/// The `f` popup: one value per `search::FILTER_FIELDS` entry, then free text.
pub struct FilterForm {
    pub values: Vec<String>,
    pub sel: usize,
    /// Terms of the query the form has no field for, kept as they are.
    other: Vec<(String, String)>,
    /// Every value the selected field takes in the library.
    options: Vec<String>,
    pub pick: usize,
}

impl FilterForm {
    /// How many suggestions the popup shows.
    pub const SHOWN: usize = 8;

    /// The options containing what is typed after the last `|`.
    pub fn suggestions(&self) -> Vec<&str> {
        let typed = self.values[self.sel].rsplit('|').next().unwrap_or_default().trim().to_lowercase();
        self.options.iter().map(String::as_str).filter(|o| o.to_lowercase().contains(&typed) && o.to_lowercase() != typed).take(Self::SHOWN).collect()
    }

    fn select(&mut self, sel: usize, refs: &[Ref]) {
        self.sel = sel;
        self.pick = 0;
        self.options = FILTER_FIELDS.get(sel).map(|(f, _)| search::suggestions(refs, f)).unwrap_or_default();
    }

    /// Replaces what is typed after the last `|` with the picked suggestion.
    fn accept(&mut self) {
        let Some(choice) = self.suggestions().get(self.pick).map(|s| s.to_string()) else { return };
        let value = &mut self.values[self.sel];
        value.truncate(value.rfind('|').map_or(0, |i| i + 1));
        value.push_str(&choice);
        self.pick = 0;
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Prompt {
    Command,
    Add,
    TagColor,
    /// A password or API key for `App::asking`; shown masked.
    Secret,
}

/// What to run again once a missing secret has been entered.
#[derive(Clone)]
pub enum Retry {
    OpenPdf,
    Command(String),
}

/// A secret the user is being asked for.
pub struct PendingSecret {
    pub account: String,
    pub label: String,
    retry: Retry,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Pane {
    Sidebar,
    List,
    Right,
}

#[derive(Clone, Copy, PartialEq)]
pub enum RightView {
    Details,
    Annotations,
    Tables,
    Graph,
}

const OPENALEX_ACCOUNT: &str = yeda_secrets::OPENALEX;

/// The citation graph shown in the right column; its selected node follows the reference list.
pub struct GraphView {
    pub graph: Graph,
    pub sel: usize,
    /// The highlighted entry of `neighbours()`.
    pub nbr: usize,
    pub scope: String,
    /// Nodes navigated away from, for Backspace.
    back: Vec<usize>,
}

impl GraphView {
    /// The selected node's neighbours: `(true, i)` for those citing it, then `(false, i)` for those it cites.
    pub fn neighbours(&self) -> Vec<(bool, usize)> {
        let (citing, cited) = self.graph.neighbours(self.sel);
        citing.into_iter().map(|i| (true, i)).chain(cited.into_iter().map(|i| (false, i))).collect()
    }
}

struct FetchJob {
    rx: Receiver<fetch::Msg>,
    location: String,
    password: Option<String>,
    added: Vec<String>,
    failed: Vec<(String, String)>,
}

struct GraphJob {
    rx: Receiver<citations::Msg>,
    command: String,
    scope: String,
    keyless: bool,
}

pub struct Batch {
    todo: std::collections::VecDeque<String>,
    total: usize,
    added: Vec<String>,
    existed: Vec<String>,
    failed: Vec<(String, String)>,
}

pub enum Job {
    Fetch(Batch),
    ExtractAnnotations,
    /// Download the selected reference's PDF from WebDAV with this password, then open it.
    Download(String),
    Attach { file: PathBuf, location: String, password: Option<String> },
}

#[derive(Clone, Copy, PartialEq)]
pub enum PickKind {
    Tags,
    Move,
}

#[derive(Clone, PartialEq)]
pub enum Pick {
    Existing(String),
    Create(String),
    Unfile,
}

/// Dropdown for editing the selected reference's tags or moving it to a collection.
pub struct Chooser {
    pub kind: PickKind,
    /// The tags being edited: those all targets share. Additions and removals are applied to every target on close.
    pub tags: Vec<String>,
    initial: Vec<String>,
    pub options: Vec<(String, Pick)>,
    pub sel: usize,
    /// Whether the user has moved the highlight, which makes Enter act even on an empty input.
    pub moved: bool,
}

impl Chooser {
    pub fn active(&self, input: &str) -> bool {
        self.moved || !input.trim().is_empty()
    }
}

enum WorkerMsg {
    Started(String),
    Finished { key: String, result: Result<(Row, Option<String>), String> },
}

struct Summarizer {
    rx: Receiver<WorkerMsg>,
    cancel: Arc<AtomicBool>,
    table: String,
    overwrite: bool,
    total: usize,
    done: usize,
    failed: Vec<String>,
    notes: Vec<String>,
}

pub struct App {
    pub dir: PathBuf,
    /// Secrets entered or read this session, by secret-store account.
    secrets: std::collections::HashMap<String, String>,
    /// Accounts whose stored secret the server rejected, so the store is skipped until a new one is entered.
    rejected: std::collections::HashSet<String>,
    pub asking: Option<PendingSecret>,
    /// The last magic-wand batch's failures, one line each.
    pub report: Vec<String>,
    pub report_copied: bool,
    clipboard: Option<arboard::Clipboard>,
    pub lib: Library,
    searcher: Searcher,
    pub query: String,
    pub mode: Mode,
    pub input: String,
    pub pane: Pane,
    pub sidebar: Vec<Filter>,
    pub side_query: String,
    pub side_visible: Vec<usize>,
    pub side_sel: usize,
    pub visible: Vec<usize>,
    pub sel: usize,
    pub page: usize,
    pending: Option<char>,
    pub status: String,
    pub quit: bool,
    pub edit: Option<PathBuf>,
    pub sync: bool,
    pub job: Option<Job>,
    picker: Picker,
    summarizer: Option<Summarizer>,
    pub chooser: Option<Chooser>,
    pub right: RightView,
    pub table_scroll: u16,
    table_scroll_key: Option<String>,
    pub annots: Vec<Annotation>,
    annots_key: Option<String>,
    annots_modified: Option<std::time::SystemTime>,
    pub annot_sel: usize,
    pub image: Option<StatefulProtocol>,
    image_for: Option<PathBuf>,
    /// Only the focused column is drawn.
    pub focus: bool,
    /// Keys of the marked references, which commands act on instead of the one under the cursor.
    pub marked: BTreeSet<String>,
    /// `V` range marking: the anchor's index in `visible`, and the marks from before it started.
    pub visual: Option<(usize, BTreeSet<String>)>,
    pub graph: Option<GraphView>,
    graph_job: Option<GraphJob>,
    fetch_job: Option<FetchJob>,
    pub sort: Sort,
    pub sort_sel: usize,
    pub form: Option<FilterForm>,
    pub help_scroll: u16,
}

impl App {
    pub fn new(dir: PathBuf, lib: Library, picker: Picker) -> Self {
        let mut app = Self {
            dir,
            sidebar: browse::sidebar(&lib),
            side_query: String::new(),
            side_visible: Vec::new(),
            lib,
            searcher: Searcher::new(),
            query: String::new(),
            mode: Mode::Normal,
            input: String::new(),
            pane: Pane::List,
            side_sel: 0,
            visible: Vec::new(),
            sel: 0,
            page: 10,
            pending: None,
            status: String::new(),
            quit: false,
            edit: None,
            sync: false,
            job: None,
            picker,
            summarizer: None,
            chooser: None,
            right: RightView::Details,
            table_scroll: 0,
            table_scroll_key: None,
            annots: Vec::new(),
            annots_key: None,
            annots_modified: None,
            annot_sel: 0,
            image: None,
            image_for: None,
            focus: false,
            marked: BTreeSet::new(),
            visual: None,
            graph: None,
            graph_job: None,
            fetch_job: None,
            sort: Sort::default(),
            sort_sel: 0,
            form: None,
            help_scroll: 0,
            secrets: Default::default(),
            rejected: Default::default(),
            asking: None,
            report: Vec::new(),
            report_copied: false,
            clipboard: None,
        };
        app.refilter_sidebar(false);
        app
    }

    pub fn selected(&self) -> Option<&Ref> {
        self.visible.get(self.sel).map(|&i| &self.lib.refs[i])
    }

    /// Indices into `lib.refs` that commands act on: the marked references, else the one under the cursor.
    pub fn targets(&self) -> Vec<usize> {
        if self.marked.is_empty() {
            return self.visible.get(self.sel).copied().into_iter().collect();
        }
        self.lib.refs.iter().enumerate().filter(|(_, r)| self.marked.contains(&r.key)).map(|(i, _)| i).collect()
    }

    fn toggle_mark(&mut self) {
        let Some(key) = self.selected().map(|r| r.key.clone()) else { return };
        if !self.marked.remove(&key) {
            self.marked.insert(key);
        }
        self.step(1);
    }

    fn extend_visual(&mut self) {
        let Some((anchor, base)) = &self.visual else { return };
        let (a, b) = (self.sel.min(*anchor), self.sel.max(*anchor));
        let mut marked = base.clone();
        marked.extend(self.visible.iter().take(b + 1).skip(a).map(|&i| self.lib.refs[i].key.clone()));
        self.marked = marked;
    }

    pub fn current_filter(&self) -> &Filter {
        self.side_visible.get(self.side_sel).map_or(&Filter::All, |&i| &self.sidebar[i])
    }

    fn refilter(&mut self) {
        let filter = self.current_filter().clone();
        self.visible = browse::visible(&mut self.searcher, &self.lib, &filter, &self.query);
        browse::sort(&self.lib, &mut self.visible, self.sort);
        self.sel = self.sel.min(self.visible.len().saturating_sub(1));
        // The anchor is an index into the old `visible`.
        self.visual = None;
    }

    /// Selects the first match when `jump_to_first`, else keeps the current entry if it still matches.
    fn refilter_sidebar(&mut self, jump_to_first: bool) {
        let current = self.current_filter().clone();
        let visible: Vec<usize> = (0..self.sidebar.len())
            .filter(|&i| i == 0 || self.side_query.is_empty() || self.searcher.fuzzy(&self.side_query, self.sidebar[i].name()))
            .collect();
        self.side_visible = visible;
        let first_match = if self.side_query.is_empty() { 0 } else { 1.min(self.side_visible.len() - 1) };
        let pos = self.side_visible.iter().position(|&i| self.sidebar[i] == current);
        self.side_sel = if jump_to_first { first_match } else { pos.unwrap_or(first_match) };
        self.refilter();
    }

    fn rebuild_sidebar(&mut self) {
        let current = self.current_filter().clone();
        self.sidebar = browse::sidebar(&self.lib);
        self.side_visible = (0..self.sidebar.len()).collect();
        self.side_sel = self.sidebar.iter().position(|f| *f == current).unwrap_or(0);
        self.refilter_sidebar(false);
    }

    fn select_filter(&mut self, f: &Filter) {
        if let Some(pos) = self.side_visible.iter().position(|&i| self.sidebar[i] == *f) {
            self.side_sel = pos;
            self.refilter();
        }
    }

    /// Applies `effects` to the library on disk; on failure, says so in the status line.
    fn apply(&mut self, effects: anyhow::Result<Vec<Effect>>) -> bool {
        match effects.and_then(|e| yeda_fs::apply(&self.dir, e)) {
            Ok(()) => true,
            Err(e) => {
                self.status = format!("{e:#}");
                false
            }
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn reload(&mut self) {
        match crate::load_library(&self.dir) {
            Ok(lib) => {
                self.lib = lib;
                self.rebuild_sidebar();
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        self.status.clear();
        match self.mode {
            Mode::Normal => self.normal_key(k),
            Mode::ConfirmSaveSecret => {
                self.mode = Mode::Normal;
                let Some(p) = self.asking.take() else { return };
                let saved = match self.secrets.get(&p.account) {
                    Some(secret) if k.code == KeyCode::Char('y') => yeda_secrets::set(&p.account, secret).err(),
                    _ => None,
                };
                self.status = if k.code == KeyCode::Char('y') { "saved" } else { "kept for this session" }.into();
                self.retry(p.retry);
                // After the retry, so its own status doesn't hide that the secret wasn't saved.
                if let Some(e) = saved {
                    self.status = format!("not saved: {e:#}; {}", self.status);
                }
            }
            Mode::ConfirmDelete => {
                self.mode = Mode::Normal;
                if k.code == KeyCode::Char('y') {
                    match self.current_filter().clone() {
                        Filter::Search(name, _) if self.pane == Pane::Sidebar => self.delete_search(&name),
                        _ => self.delete_selected(),
                    }
                }
            }
            Mode::Pick => self.pick_key(k),
            Mode::Filter => self.filter_key(k),
            Mode::Sort => self.sort_key(k),
            Mode::Help(_) => self.help_key(k),
            Mode::Search | Mode::SideSearch | Mode::Prompt(_) => self.input_key(k),
        }
    }

    fn normal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page / 2).max(1) as isize;
        match (k.code, self.pending.take()) {
            (KeyCode::Char('d'), _) if ctrl => self.step(half),
            (KeyCode::Char('u'), _) if ctrl => self.step(-half),
            (KeyCode::Char('g'), Some('g')) => self.move_to(0),
            (KeyCode::Char('d'), Some('d')) => match (self.pane, self.current_filter()) {
                (Pane::Sidebar, Filter::Search(name, _)) => {
                    self.status = format!("delete saved search '{name}'? (y/n)");
                    self.mode = Mode::ConfirmDelete;
                }
                (Pane::List, _) => match self.targets()[..] {
                    [] => {}
                    [i] => {
                        self.status = format!("delete {}? (y/n)", self.lib.refs[i].key);
                        self.mode = Mode::ConfirmDelete;
                    }
                    ref many => {
                        self.status = format!("delete {} marked references? (y/n)", many.len());
                        self.mode = Mode::ConfirmDelete;
                    }
                },
                _ => {}
            },
            (KeyCode::Char(c @ ('g' | 'd')), None) => self.pending = Some(c),
            (KeyCode::Char('q'), _) => self.quit = true,
            (KeyCode::Char('j') | KeyCode::Down, _) => self.step(1),
            (KeyCode::Char('k') | KeyCode::Up, _) => self.step(-1),
            (KeyCode::Char('G'), _) => self.move_to(usize::MAX),
            (KeyCode::Char('h') | KeyCode::Left, _) => {
                self.pane = if self.pane == Pane::Right { Pane::List } else { Pane::Sidebar }
            }
            (KeyCode::Enter, _) if self.in_graph() => self.graph_go(),
            (KeyCode::Backspace, _) if self.in_graph() => self.graph_back(),
            (KeyCode::Char('a'), _) if self.in_graph() => self.graph_add(),
            (KeyCode::Char('o'), _) if self.in_graph() => self.graph_open(),
            (KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter, _) => {
                let right_focusable = self.right != RightView::Details;
                self.pane = if self.pane == Pane::Sidebar || !right_focusable { Pane::List } else { Pane::Right }
            }
            (KeyCode::Tab, _) => {
                self.pane = match self.pane {
                    Pane::Sidebar => Pane::List,
                    Pane::List if self.right != RightView::Details => Pane::Right,
                    _ => Pane::Sidebar,
                }
            }
            (KeyCode::Char('A'), _) => {
                self.toggle_right(RightView::Annotations);
            }
            (KeyCode::Char('T'), _) => {
                self.toggle_right(RightView::Tables);
            }
            (KeyCode::Char('x'), _) if !self.targets().is_empty() => {
                self.status = "extracting annotations…".into();
                self.job = Some(Job::ExtractAnnotations);
            }
            (KeyCode::Char('/'), _) if self.pane == Pane::Sidebar => self.mode = Mode::SideSearch,
            (KeyCode::Char('/'), _) => {
                self.pane = Pane::List;
                self.mode = Mode::Search;
            }
            (KeyCode::Char('v'), _) if self.pane == Pane::List => self.toggle_mark(),
            (KeyCode::Char('V'), _) if self.pane == Pane::List => {
                self.visual = match self.visual {
                    Some(_) => None,
                    None => Some((self.sel, self.marked.clone())),
                };
                self.extend_visual();
            }
            (KeyCode::Esc, _) if self.visual.is_some() => self.visual = None,
            (KeyCode::Esc, _) if self.pane == Pane::List && !self.marked.is_empty() => {
                self.status = format!("unmarked {}", self.marked.len());
                self.marked.clear();
            }
            (KeyCode::Esc, _) if self.pane == Pane::Sidebar => {
                self.side_query.clear();
                self.refilter_sidebar(false);
            }
            (KeyCode::Esc, _) => {
                self.query.clear();
                self.refilter();
            }
            (KeyCode::Char('r'), _) => self.cycle_status(true),
            (KeyCode::Char('R'), _) => self.cycle_status(false),
            (KeyCode::Char('c'), _) => self.focus = !self.focus,
            (KeyCode::Char('?'), _) => self.open_help(HelpPage::Keys),
            (KeyCode::Char('f'), _) => self.open_filter(),
            (KeyCode::Char('s'), _) => {
                self.sort_sel = SortField::ALL.iter().position(|f| *f == self.sort.field).unwrap_or(0);
                self.mode = Mode::Sort;
            }
            (KeyCode::Char('C'), _) => match self.current_filter() {
                Filter::Tag(t) if self.pane == Pane::Sidebar => {
                    self.input = self.lib.tag_colors.get(t).cloned().unwrap_or_default();
                    self.mode = Mode::Prompt(Prompt::TagColor);
                }
                _ => self.status = "select a tag in the library pane to color it".into(),
            },
            (KeyCode::Char('o'), _) => self.open_pdf(),
            (KeyCode::Char('e'), _) => self.edit = self.selected().map(|r| self.path(&library::ref_path(&r.key))),
            (KeyCode::Char('n'), _) => self.edit = self.selected().map(|r| self.path(&library::note_path(&r.key))),
            (KeyCode::Char('t'), _) => self.open_chooser(PickKind::Tags),
            (KeyCode::Char('m'), _) => self.open_chooser(PickKind::Move),
            (KeyCode::Char(':'), _) => self.mode = Mode::Prompt(Prompt::Command),
            (KeyCode::Char('a'), _) => self.mode = Mode::Prompt(Prompt::Add),
            _ => {}
        }
    }

    /// Types pasted text into the open prompt; ignored in modes where letters are commands.
    pub fn on_paste(&mut self, text: &str) {
        let add = matches!(self.mode, Mode::Prompt(Prompt::Add));
        if !matches!(self.mode, Mode::Prompt(_) | Mode::Search | Mode::SideSearch | Mode::Pick | Mode::Filter) {
            return;
        }
        // Lines separate identifiers in a pasted list; elsewhere a newline would only end the prompt.
        // Terminals differ in sending line breaks as \r, \n or both.
        for c in text.replace("\r\n", "\n").replace('\r', "\n").trim_end().chars() {
            let c = if c == '\n' && !add { ' ' } else { c };
            self.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn input_key(&mut self, k: KeyEvent) {
        let mode = self.mode;
        let buf = match mode {
            Mode::Search => &mut self.query,
            Mode::SideSearch => &mut self.side_query,
            _ => &mut self.input,
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('u') if ctrl => buf.clear(),
            KeyCode::Char('w') if ctrl => {
                let keep = buf.trim_end().rfind(' ').map_or(0, |i| i + 1);
                buf.truncate(keep);
            }
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Esc => {
                buf.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if let Mode::Prompt(p) = std::mem::replace(&mut self.mode, Mode::Normal) {
                    let text = std::mem::take(&mut self.input);
                    self.submit(p, text.trim());
                }
            }
            _ => {}
        }
        match mode {
            Mode::Search => self.refilter(),
            Mode::SideSearch => self.refilter_sidebar(true),
            _ => {}
        }
    }

    fn open_filter(&mut self) {
        let q = search::parse(&self.query);
        let mut values = vec![String::new(); FILTER_FIELDS.len()];
        let mut other = Vec::new();
        for (name, value) in q.fields {
            let canonical = match name.as_str() {
                "authors" => "author",
                "collection" => "coll",
                n => n,
            };
            match FILTER_FIELDS.iter().position(|(f, _)| *f == canonical) {
                Some(i) if values[i].is_empty() && !value.is_empty() => values[i] = value,
                _ => other.push((name, value)),
            }
        }
        values.push(q.free.replace("\\ ", " "));
        let mut form = FilterForm { values, sel: 0, other, options: Vec::new(), pick: 0 };
        form.select(0, &self.lib.refs);
        self.form = Some(form);
        self.mode = Mode::Filter;
    }

    fn filter_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(form) = &mut self.form else { return };
        let n = form.values.len();
        match k.code {
            KeyCode::Esc => {
                self.form = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let mut fields = std::mem::take(&mut form.other);
                for ((name, _), value) in FILTER_FIELDS.iter().zip(&form.values) {
                    if !value.trim().is_empty() {
                        fields.push((name.to_string(), value.trim().to_string()));
                    }
                }
                self.query = search::compose(&fields, &form.values[n - 1]);
                self.form = None;
                self.mode = Mode::Normal;
                self.pane = Pane::List;
                self.refilter();
            }
            KeyCode::Down | KeyCode::Tab => form.select((form.sel + 1) % n, &self.lib.refs),
            KeyCode::Up | KeyCode::BackTab => form.select((form.sel + n - 1) % n, &self.lib.refs),
            KeyCode::Char('n') if ctrl => form.pick = (form.pick + 1).min(form.suggestions().len().saturating_sub(1)),
            KeyCode::Char('p') if ctrl => form.pick = form.pick.saturating_sub(1),
            KeyCode::Right => form.accept(),
            KeyCode::Char('u') if ctrl => form.values[form.sel].clear(),
            KeyCode::Backspace => {
                form.values[form.sel].pop();
                form.pick = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                form.values[form.sel].push(c);
                form.pick = 0;
            }
            _ => {}
        }
    }

    /// Enter on the current sort field flips its direction.
    fn sort_key(&mut self, k: KeyEvent) {
        let n = SortField::ALL.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => self.sort_sel = (self.sort_sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => self.sort_sel = (self.sort_sel + n - 1) % n,
            KeyCode::Enter => {
                let field = SortField::ALL[self.sort_sel];
                self.sort = Sort { field, descending: field == self.sort.field && !self.sort.descending };
                self.mode = Mode::Normal;
                self.refilter();
            }
            _ => {}
        }
    }

    fn open_help(&mut self, page: HelpPage) {
        self.help_scroll = 0;
        self.mode = Mode::Help(page);
    }

    fn help_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page / 2).max(1) as u16;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Char('d') if ctrl => self.help_scroll = self.help_scroll.saturating_add(half),
            KeyCode::Char('u') if ctrl => self.help_scroll = self.help_scroll.saturating_sub(half),
            KeyCode::Char('j') | KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::Char('G') => self.help_scroll = u16::MAX,
            _ => {}
        }
    }

    fn open_chooser(&mut self, kind: PickKind) {
        let targets = self.targets();
        let Some(&first) = targets.first() else {
            self.status = "no reference selected".into();
            return;
        };
        let refs = &self.lib.refs;
        let tags: Vec<String> = refs[first].tags.iter().filter(|t| targets.iter().all(|&i| refs[i].tags.contains(t))).cloned().collect();
        self.chooser = Some(Chooser { kind, initial: tags.clone(), tags, options: vec![], sel: 0, moved: false });
        self.input.clear();
        self.mode = Mode::Pick;
        self.refresh_options();
    }

    /// Rebuilds the dropdown from the input: fuzzy matches ranked by score, then create/remove entries.
    fn refresh_options(&mut self) {
        let current_coll = self.selected().and_then(|r| r.collections.first().cloned());
        let Some(c) = &mut self.chooser else { return };
        let query = self.input.trim().to_string();
        let mut candidates: Vec<String> = match c.kind {
            PickKind::Tags => c.tags.clone(),
            PickKind::Move => vec![],
        };
        for f in &self.sidebar {
            match (c.kind, f) {
                (PickKind::Tags, Filter::Tag(t)) if !candidates.contains(t) => candidates.push(t.clone()),
                (PickKind::Move, Filter::Collection(p)) => candidates.push(p.clone()),
                _ => {}
            }
        }
        let mut scored: Vec<(u32, &String)> = candidates
            .iter()
            .filter_map(|t| if query.is_empty() { Some((0, t)) } else { self.searcher.score(&query, t).map(|s| (s, t)) })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        c.options = scored
            .into_iter()
            .map(|(_, t)| {
                let mark = match c.kind {
                    PickKind::Tags if c.tags.contains(t) => "✓ ",
                    PickKind::Move if current_coll.as_ref() == Some(t) => "• ",
                    _ => "  ",
                };
                (format!("{mark}{t}"), Pick::Existing(t.clone()))
            })
            .collect();
        if !query.is_empty() && !candidates.contains(&query) {
            let what = if c.kind == PickKind::Tags { "tag" } else { "collection" };
            c.options.push((format!("+ new {what} \"{query}\""), Pick::Create(query)));
        }
        if c.kind == PickKind::Move && current_coll.is_some() {
            c.options.push(("— remove from its collection".into(), Pick::Unfile));
        }
        c.sel = 0;
        c.moved = false;
    }

    fn pick_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(c) = &mut self.chooser else { return };
        let step = |c: &mut Chooser, delta: isize| {
            if !c.options.is_empty() {
                let n = c.options.len() as isize;
                c.sel = (c.sel as isize + delta).rem_euclid(n) as usize;
                c.moved = true;
            }
        };
        match k.code {
            KeyCode::Esc => self.close_chooser(),
            KeyCode::Down | KeyCode::Tab => step(c, 1),
            KeyCode::Char('n') if ctrl => step(c, 1),
            KeyCode::Up | KeyCode::BackTab => step(c, -1),
            KeyCode::Char('p') if ctrl => step(c, -1),
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.refresh_options();
            }
            KeyCode::Backspace => {
                if self.input.is_empty() && c.kind == PickKind::Tags {
                    c.tags.pop();
                } else {
                    self.input.pop();
                }
                self.refresh_options();
            }
            KeyCode::Char(ch) if !ctrl => {
                self.input.push(ch);
                self.refresh_options();
            }
            KeyCode::Enter if !c.active(&self.input) => self.close_chooser(),
            KeyCode::Enter => {
                let Some((_, choice)) = c.options.get(c.sel).cloned() else { return };
                match (c.kind, choice) {
                    (PickKind::Tags, Pick::Existing(t) | Pick::Create(t)) => {
                        match c.tags.iter().position(|x| *x == t) {
                            Some(i) => {
                                c.tags.remove(i);
                            }
                            None => c.tags.push(t),
                        }
                        self.input.clear();
                        self.refresh_options();
                    }
                    (PickKind::Move, choice) => {
                        let collections = match choice {
                            Pick::Existing(p) | Pick::Create(p) => vec![p],
                            Pick::Unfile => vec![],
                        };
                        self.update_targets(|r| r.collections = collections.clone());
                        self.close_chooser();
                    }
                    (PickKind::Tags, Pick::Unfile) => {}
                }
            }
            _ => {}
        }
    }

    /// Saves edited tags (a move is applied on Enter, so closing a move picker changes nothing).
    fn close_chooser(&mut self) {
        self.mode = Mode::Normal;
        self.input.clear();
        if let Some(c) = self.chooser.take()
            && c.kind == PickKind::Tags
            && c.tags != c.initial
        {
            let removed: Vec<_> = c.initial.iter().filter(|t| !c.tags.contains(t)).collect();
            let added: Vec<_> = c.tags.iter().filter(|t| !c.initial.contains(t)).collect();
            self.update_targets(|r| {
                r.tags.retain(|t| !removed.contains(&t));
                for t in &added {
                    if !r.tags.contains(t) {
                        r.tags.push(t.to_string());
                    }
                }
            });
        }
    }

    fn update_targets(&mut self, change: impl Fn(&mut Ref)) {
        let targets = self.targets();
        let effects = targets.iter().map(|&i| {
            change(&mut self.lib.refs[i]);
            self.lib.save(&self.lib.refs[i])
        });
        let effects = effects.collect::<anyhow::Result<Vec<_>>>();
        let status = match targets[..] {
            [i] => {
                let r = &self.lib.refs[i];
                format!("{}: tags [{}], collection {}", r.key, r.tags.join(", "), r.collections.first().map_or("none", String::as_str))
            }
            _ => format!("updated {} references", targets.len()),
        };
        if self.apply(effects) {
            self.status = status;
        }
        self.rebuild_sidebar();
    }

    fn submit(&mut self, p: Prompt, text: &str) {
        match p {
            Prompt::Command => match text.split_once(' ').unwrap_or((text, "")) {
                ("q" | "q!" | "quit", _) => self.quit = true,
                ("save", name) if !name.trim().is_empty() => self.save_search(name.trim()),
                ("save", _) => self.status = "usage: :save <name>".into(),
                (cmd @ ("summarize" | "summarize!" | "summarize-all" | "summarize-all!"), args) => {
                    if let Err(e) = self.summarize(args, cmd.starts_with("summarize-all"), cmd.ends_with('!'), text) {
                        self.status = format!("{e:#}");
                    }
                }
                ("help", _) => self.open_help(HelpPage::Guide),
                ("keys", _) => self.open_help(HelpPage::Keys),
                ("failed", _) => self.open_help(HelpPage::Failed),
                ("cancel", _) => match &self.summarizer {
                    Some(s) => {
                        s.cancel.store(true, Ordering::Relaxed);
                        self.status = "cancelling after the current paper…".into();
                    }
                    None => self.status = "nothing to cancel".into(),
                },
                ("export", args) => self.export(args),
                ("attach", args) => self.attach(args, text),
                ("sync", _) => self.sync = true,
                (cmd @ ("graph" | "graph!"), args) => self.graph_command(args, cmd.ends_with('!'), text),
                ("fetch", args) => self.fetch_command(args.trim(), text),
                ("cite", args) => self.cite(args.trim()),
                ("", _) => {}
                _ => self.status = format!("unknown command: {text}"),
            },
            Prompt::TagColor => {
                let Filter::Tag(tag) = self.current_filter().clone() else { return };
                if !text.is_empty() && text.parse::<ratatui::style::Color>().is_err() {
                    self.status = format!("not a color: {text} (use #rrggbb or a name like red)");
                    return;
                }
                let result = self.lib.update_config(|t| {
                    let colors = config_section(t, "tag_colors")?;
                    if text.is_empty() {
                        colors.remove(&tag);
                    } else {
                        colors.insert(tag.clone(), text.into());
                    }
                    Ok(())
                });
                self.after_config_change(result);
            }
            Prompt::Secret => {
                let Some(p) = &self.asking else { return };
                if text.is_empty() {
                    self.asking = None;
                    self.status = "cancelled".into();
                    return;
                }
                self.secrets.insert(p.account.clone(), text.into());
                self.rejected.remove(&p.account);
                self.status = format!("save {} in the secret store? (y/n)", p.label);
                self.mode = Mode::ConfirmSaveSecret;
            }
            Prompt::Add if !ids::split_ids(text).is_empty() => {
                let todo: std::collections::VecDeque<String> = ids::split_ids(text).into_iter().map(Into::into).collect();
                self.status = format!("fetching 1/{}: {}…", todo.len(), todo[0]);
                self.job = Some(Job::Fetch(Batch { total: todo.len(), todo, added: vec![], existed: vec![], failed: vec![] }));
            }
            _ => {}
        }
    }

    fn after_config_change(&mut self, result: anyhow::Result<Effect>) {
        if self.apply(result.map(|e| vec![e])) {
            self.reload();
        }
    }

    fn save_search(&mut self, name: &str) {
        if self.query.trim().is_empty() {
            self.status = "nothing to save: search with / first".into();
            return;
        }
        let query = std::mem::take(&mut self.query);
        let result = self.lib.update_config(|t| {
            config_section(t, "searches")?.insert(name.into(), query.clone().into());
            Ok(())
        });
        self.after_config_change(result);
        self.side_query.clear();
        self.refilter_sidebar(false);
        self.select_filter(&Filter::Search(name.into(), query));
        self.status = format!("saved search '{name}'");
    }

    fn delete_search(&mut self, name: &str) {
        let result = self.lib.update_config(|t| {
            config_section(t, "searches")?.remove(name);
            Ok(())
        });
        self.after_config_change(result);
    }

    /// Every target gets the status after the current reference's, so marked references end up alike.
    fn cycle_status(&mut self, forward: bool) {
        let targets = self.targets();
        let cur = self.visible.get(self.sel).copied().filter(|i| targets.contains(i)).or(targets.first().copied());
        let Some(cur) = cur else { return };
        let mut probe = self.lib.refs[cur].clone();
        library::cycle_status(&mut probe, forward);
        self.update_targets(|r| r.status = probe.status.clone());
        self.status = format!("status: {}{}", probe.status.as_deref().unwrap_or("none"), plural_suffix(targets.len()));
        self.refilter();
    }

    fn table_name(&self, arg: Option<&str>) -> anyhow::Result<String> {
        let names: Vec<&String> = self.lib.tables.keys().collect();
        match arg {
            Some(n) if self.lib.tables.contains_key(n) => Ok(n.to_string()),
            Some(n) => anyhow::bail!("no table named '{n}' (tables: {})", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
            None if names.len() == 1 => Ok(names[0].clone()),
            None if names.is_empty() => anyhow::bail!("no tables defined: add [tables.<name>] to yeda.toml or run import-table"),
            None => anyhow::bail!("several tables; name one: {}", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }

    /// `args` is `[table] [llm profile]`; `all` covers every visible reference instead of the selected one.
    /// `command` is the whole command line, run again once a missing API key is entered.
    fn summarize(&mut self, args: &str, all: bool, overwrite: bool, command: &str) -> anyhow::Result<()> {
        if self.summarizer.is_some() {
            anyhow::bail!("a summarization is already running (:cancel stops it)");
        }
        let mut words = args.split_whitespace();
        let table_name = self.table_name(words.next())?;
        let (profile_name, profile) = self.lib.llm_profile(words.next())?;
        let table = self.lib.tables[&table_name].clone();
        let targets: Vec<usize> = if all { self.visible.clone() } else { self.targets() };
        if targets.is_empty() {
            anyhow::bail!("no reference selected");
        }
        let jobs: Vec<tables::Job> = targets
            .iter()
            .filter_map(|&i| {
                let r = &self.lib.refs[i];
                tables::plan(&self.lib, &table_name, &table, r, &self.load_annotations(&r.key), overwrite)
            })
            .collect();
        if jobs.is_empty() {
            anyhow::bail!("nothing to fill in '{table_name}' (add ! to regenerate existing cells)");
        }
        let env = profile.key_env().to_string();
        let key = std::env::var(&env).ok().filter(|k| !k.is_empty());
        let key = match key {
            Some(k) => Some(k),
            None if profile.needs_key() => {
                let Some(k) = self.secret(&yeda_secrets::llm_account(&env), &env, Retry::Command(command.into())) else {
                    return Ok(());
                };
                Some(k)
            }
            None => yeda_secrets::get(&yeda_secrets::llm_account(&env)).ok().flatten(),
        };
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let total = jobs.len();
        std::thread::spawn(move || {
            for (i, job) in jobs.into_iter().enumerate() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(WorkerMsg::Started(format!("summarizing {} ({}/{total}) with {profile_name}…", job.key, i + 1)));
                let bytes = job.pdf.as_ref().and_then(|p| fs::read(p).ok());
                let path = job.pdf.clone().unwrap_or_default();
                let text = || poppler::pdf_text(&path);
                let paper = bytes.as_deref().map(|pdf| Paper { pdf, text: &text });
                let prompt = LlmPrompt { system: job.system, text: job.text, paper, schema: job.schema };
                let result = llm::complete(&profile, &prompt, key.as_deref())
                    .and_then(|reply| Ok((tables::parse_row(&reply.text, &job.columns)?, reply.note)))
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(WorkerMsg::Finished { key: job.key, result });
            }
        });
        self.summarizer = Some(Summarizer {
            rx,
            cancel,
            table: table_name,
            overwrite,
            total,
            done: 0,
            failed: vec![],
            notes: vec![],
        });
        Ok(())
    }

    /// Applies finished summaries; called from the event loop.
    pub fn poll_worker(&mut self) {
        let Some(s) = &mut self.summarizer else { return };
        loop {
            match s.rx.try_recv() {
                Ok(WorkerMsg::Started(text)) => self.status = text,
                Ok(WorkerMsg::Finished { key, result: Ok((row, note)) }) => {
                    if let Some(i) = self.lib.refs.iter().position(|r| r.key == key) {
                        tables::merge(&mut self.lib.refs[i], &s.table, row, s.overwrite);
                        let saved = self.lib.save(&self.lib.refs[i]).and_then(|e| yeda_fs::apply(&self.dir, [e]));
                        if let Err(e) = saved {
                            s.failed.push(format!("{key}: {e:#}"));
                        }
                    }
                    s.done += 1;
                    s.notes.extend(note.map(|n| format!("{key}: {n}")));
                }
                Ok(WorkerMsg::Finished { key, result: Err(e) }) => s.failed.push(format!("{key}: {e}")),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    let mut msg = format!("summarized {}/{} into '{}'", s.done, s.total, s.table);
                    if !s.failed.is_empty() {
                        msg += &format!("; {} failed: {}", s.failed.len(), s.failed.join(" | "));
                    }
                    if !s.notes.is_empty() {
                        msg += &format!("; {}", s.notes.join(" | "));
                    }
                    self.status = msg;
                    self.summarizer = None;
                    return;
                }
            }
        }
    }

    fn export(&mut self, args: &str) {
        let Some((table, path)) = args.trim().split_once(' ') else {
            self.status = "usage: :export <table> <file.csv>".into();
            return;
        };
        let path = path.trim();
        let path = match (path.strip_prefix("~/"), dirs::home_dir()) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        };
        let written = tables::export_csv(&self.lib, table)
            .and_then(|(csv, n)| fs::write(&path, csv).map(|()| n).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())));
        self.status = match written {
            Ok(n) => format!("exported {n} rows of '{table}' to {}", path.display()),
            Err(e) => format!("{e:#}"),
        };
    }

    pub fn run_job(&mut self, job: Job) {
        match job {
            Job::Fetch(batch) => self.add_next(batch),
            Job::ExtractAnnotations => self.extract_annotations(),
            Job::Download(password) => self.download(&password),
            Job::Attach { file, location, password } => self.finish_attach(&file, &location, password.as_deref()),
        }
    }

    /// The secret for `account`: from this session, else the secret store, else a prompt, after which
    /// `retry` runs again. `None` while prompting.
    fn secret(&mut self, account: &str, label: &str, retry: Retry) -> Option<String> {
        if let Some(s) = self.secrets.get(account) {
            return Some(s.clone());
        }
        if !self.rejected.contains(account) {
            match yeda_secrets::get(account) {
                Ok(Some(s)) => {
                    self.secrets.insert(account.into(), s.clone());
                    return Some(s);
                }
                Ok(None) => {}
                Err(e) => self.status = format!("{e:#}; "),
            }
        }
        self.asking = Some(PendingSecret { account: account.into(), label: label.into(), retry });
        self.input.clear();
        self.mode = Mode::Prompt(Prompt::Secret);
        None
    }

    /// Forgets a secret the server turned down, so the next attempt asks again.
    fn reject(&mut self, account: &str) {
        self.secrets.remove(account);
        self.rejected.insert(account.into());
    }

    fn retry(&mut self, retry: Retry) {
        match retry {
            Retry::OpenPdf => self.open_pdf(),
            Retry::Command(text) => self.submit(Prompt::Command, &text),
        }
    }

    fn download(&mut self, password: &str) {
        let Some(r) = self.selected() else { return };
        let (Some(remote), Ok(path)) = (self.lib.remote_pdf(r), self.lib.pdf_path(r)) else { return };
        let account = yeda_secrets::webdav_account(&remote.dav.user, &remote.dav.url);
        let saved = yeda_net::webdav::download(remote.dav, password, &remote.key, &remote.file)
            .and_then(|pdf| Ok(yeda_fs::write_atomic(&path, &pdf)?));
        match saved {
            Ok(()) => self.open_pdf(),
            Err(e) if yeda_net::webdav::is_unauthorized(&e) => {
                self.reject(&account);
                self.open_pdf();
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// `:attach <file.pdf> [location]`; `command` is the whole line, run again once a password is entered.
    fn attach(&mut self, args: &str, command: &str) {
        let args = args.trim();
        let (file, location) = match args.rsplit_once(' ') {
            Some((f, l)) if self.lib.locations.contains_key(l) || self.lib.remotes.contains_key(l) => (f.trim(), Some(l)),
            _ => (args, None),
        };
        if file.is_empty() || self.selected().is_none() {
            self.status = "usage: :attach <file.pdf> [location], with a reference selected".into();
            return;
        }
        let names: Vec<&String> = self.lib.locations.keys().chain(self.lib.remotes.keys()).collect();
        let location = match (location, names.as_slice()) {
            (Some(l), _) => l.to_string(),
            (None, [only]) => only.to_string(),
            (None, _) => {
                let names: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                self.status = format!("name a location: {}", names.join(", "));
                return;
            }
        };
        let file = match (file.strip_prefix("~/"), dirs::home_dir()) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(file),
        };
        let password = match self.lib.remotes.get(&location) {
            Some(dav) => {
                let account = yeda_secrets::webdav_account(&dav.user, &dav.url);
                let label = format!("{}@{}", dav.user, dav.url);
                match self.secret(&account, &label, Retry::Command(command.into())) {
                    Some(p) => Some(p),
                    None => return,
                }
            }
            None => None,
        };
        self.status = format!("attaching {}…", file.display());
        self.job = Some(Job::Attach { file, location, password });
    }

    /// Copies `file` to a local location, or uploads it to a WebDAV one keeping a cached copy;
    /// returns reference `i` pointing at it, not yet saved.
    fn attach_file(&self, i: usize, file: &std::path::Path, location: &str, password: Option<&str>) -> anyhow::Result<Ref> {
        let bytes = fs::read(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
        let mut r = self.lib.refs[i].clone();
        r.pdf = Some(self.lib.attach(&r, location, &file.to_string_lossy())?);
        if let (Some(remote), Some(password)) = (self.lib.remote_pdf(&r), password) {
            let mtime = fs::metadata(file)?.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
            yeda_net::webdav::upload(remote.dav, password, &remote.key, &remote.file, &bytes, mtime)?;
        }
        yeda_fs::write_atomic(&self.lib.pdf_path(&r)?, &bytes)?;
        Ok(r)
    }

    fn finish_attach(&mut self, file: &std::path::Path, location: &str, password: Option<&str>) {
        let Some(&i) = self.visible.get(self.sel) else { return };
        match self.attach_file(i, file, location, password) {
            Ok(r) => {
                let path = r.pdf.as_ref().map(|p| format!("{}:{}", p.location, p.path)).unwrap_or_default();
                if self.apply(self.lib.save(&r).map(|e| vec![e])) {
                    self.status = format!("attached {path}");
                    self.reload();
                }
            }
            Err(e) if yeda_net::webdav::is_unauthorized(&e) => {
                if let Some(dav) = self.lib.remotes.get(location) {
                    let account = yeda_secrets::webdav_account(&dav.user, &dav.url);
                    self.reject(&account);
                }
                self.status = format!("{e:#}; run :attach again to enter the password");
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    fn extract_annotations(&mut self) {
        let targets = self.targets();
        let mut extracted = 0;
        let mut failed = Vec::new();
        for &i in &targets {
            match self.extract_from(i) {
                Ok(n) => extracted += n,
                Err(e) if targets.len() == 1 => failed.push(format!("{e:#}")),
                Err(e) => failed.push(format!("{}: {e:#}", self.lib.refs[i].key)),
            }
        }
        self.status = match (failed.is_empty(), targets.len()) {
            (true, 1) => format!("{extracted} annotations extracted from pdf"),
            (true, n) => format!("{extracted} annotations extracted from {n} pdfs"),
            (false, _) => failed.join(" · "),
        };
        self.annots_key = None;
        self.image_for = None;
        self.right = RightView::Annotations;
        self.pane = Pane::Right;
    }

    fn extract_from(&mut self, i: usize) -> anyhow::Result<usize> {
        let r = &self.lib.refs[i];
        let key = r.key.clone();
        let result = self.lib.pdf_path(r).and_then(|pdf| {
            let bytes = fs::read(&pdf).map_err(|e| anyhow::anyhow!("{}: {e}", pdf.display()))?;
            let existing = yeda_fs::read_optional(&self.path(&annotations::file_path(&key)))?;
            let words = poppler::word_boxes(&pdf)?;
            let mut render = |page, height, rect| poppler::render_region(&pdf, page, height, rect).ok();
            annotations::sync_from_pdf(&library::annotations_dir(&key), existing.as_deref(), &bytes, Some(&words), &mut render)
        });
        let (n, effects) = result?;
        yeda_fs::apply(&self.dir, effects)?;
        Ok(n)
    }

    fn load_annotations(&self, key: &str) -> Vec<Annotation> {
        let text = yeda_fs::read_optional(&self.path(&annotations::file_path(key))).ok().flatten();
        text.and_then(|t| annotations::parse(&t).ok()).unwrap_or_default()
    }

    /// Loads the selected ref's annotations and the selected annotation's image, if they changed.
    pub fn sync_annots(&mut self) {
        let key = self.selected().map(|r| r.key.clone());
        let modified = key.as_ref().and_then(|k| fs::metadata(self.path(&annotations::file_path(k))).ok()?.modified().ok());
        if key != self.annots_key || modified != self.annots_modified {
            self.annots_modified = modified;
            let same_paper = key == self.annots_key;
            self.annots = match &key {
                Some(k) => {
                    let text = yeda_fs::read_optional(&self.path(&annotations::file_path(k)));
                    text.and_then(|t| t.as_deref().map(annotations::parse).transpose()).map(Option::unwrap_or_default).unwrap_or_else(|e| {
                        self.status = format!("{e:#}");
                        vec![]
                    })
                }
                None => vec![],
            };
            self.annots_key = key;
            if same_paper {
                // The file changed on disk (e.g. yeda-reader saved): keep the selection, re-read the image.
                self.annot_sel = self.annot_sel.min(self.annots.len().saturating_sub(1));
                self.image_for = None;
            } else {
                self.annot_sel = 0;
            }
        }
        let want = match (&self.annots_key, self.annots.get(self.annot_sel)) {
            (Some(k), Some(Annotation { image: Some(img), .. })) => Some(self.path(&library::annotations_dir(k)).join(img)),
            _ => None,
        };
        if want != self.image_for {
            self.image = want.as_ref().and_then(|p| match image::open(p) {
                Ok(img) => Some(self.picker.new_resize_protocol(img)),
                Err(e) => {
                    self.status = format!("{}: {e}", p.display());
                    None
                }
            });
            self.image_for = want;
        }
    }

    /// Fetches and saves one identifier; returns its key and whether it was already in the library.
    fn add_one(&mut self, input: &str) -> anyhow::Result<(String, bool)> {
        let mut r = wand::fetch(input)?;
        if let Some(x) = r.doi.as_deref().and_then(|d| self.lib.find_doi(d)) {
            return Ok((x.key.clone(), true));
        }
        r.key = self.lib.unique_key(&r);
        yeda_fs::apply(&self.dir, vec![self.lib.save(&r)?])?;
        self.reload();
        Ok((r.key, false))
    }

    /// Adds the batch's next identifier, then queues the rest so the status line shows progress.
    fn add_next(&mut self, mut batch: Batch) {
        let Some(input) = batch.todo.pop_front() else { return self.finish_batch(batch) };
        match self.add_one(&input) {
            Ok((key, true)) => batch.existed.push(key),
            Ok((key, false)) => batch.added.push(key),
            Err(e) => batch.failed.push((input, format!("{e:#}"))),
        }
        match batch.todo.front() {
            Some(next) => {
                self.status = format!("fetching {}/{}: {next}…", batch.total - batch.todo.len() + 1, batch.total);
                self.job = Some(Job::Fetch(batch));
            }
            None => self.finish_batch(batch),
        }
    }

    fn finish_batch(&mut self, batch: Batch) {
        let mut parts = Vec::new();
        if !batch.added.is_empty() {
            parts.push(format!("added {}", batch.added.join(", ")));
        }
        if !batch.existed.is_empty() {
            parts.push(format!("already in library: {}", batch.existed.join(", ")));
        }
        if !batch.failed.is_empty() {
            parts.push(self.report_failures(&batch.failed));
        }
        self.status = parts.join(" · ");
        let Some(key) = batch.added.last().or(batch.existed.last()).cloned() else {
            if !batch.failed.is_empty() {
                self.open_help(HelpPage::Failed);
            }
            return;
        };
        self.query.clear();
        self.side_query.clear();
        self.refilter_sidebar(false);
        self.select_filter(&Filter::All);
        self.pane = Pane::List;
        if let Some(pos) = self.visible.iter().position(|&i| self.lib.refs[i].key == key) {
            self.sel = pos;
        }
        if !batch.failed.is_empty() {
            self.open_help(HelpPage::Failed);
        }
    }

    /// Copies the failed inputs to the clipboard and keeps them for the `:failed` popup; returns the status part.
    fn report_failures(&mut self, failed: &[(String, String)]) -> String {
        let inputs: Vec<&str> = failed.iter().map(|(i, _)| i.as_str()).collect();
        let copy = self.copy(&inputs.join("\n"));
        self.report_copied = copy.is_ok();
        let copied = match copy {
            Ok(()) => "copied to the clipboard".to_string(),
            Err(e) => format!("clipboard: {e}"),
        };
        self.report = failed.iter().map(|(i, e)| format!("{i}  {e}")).collect();
        format!("{} failed ({copied})", inputs.len())
    }

    /// Keeps the clipboard open for the session: on Linux its contents last only while it is.
    fn copy(&mut self, text: &str) -> Result<(), arboard::Error> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new()?);
        }
        self.clipboard.as_mut().unwrap().set_text(text)
    }

    fn toggle_right(&mut self, view: RightView) {
        self.right = if self.right == view { RightView::Details } else { view };
        self.pane = if self.right == RightView::Details { Pane::List } else { Pane::Right };
    }

    /// Scrolls the tables view back to the top when the selected reference changes.
    pub fn sync_table_scroll(&mut self) {
        let key = self.selected().map(|r| r.key.clone());
        if key != self.table_scroll_key {
            self.table_scroll = 0;
            self.table_scroll_key = key;
        }
    }

    fn step(&mut self, delta: isize) {
        let cur = match self.pane {
            Pane::Sidebar => self.side_sel,
            Pane::List => self.sel,
            Pane::Right if self.right == RightView::Tables => self.table_scroll as usize,
            Pane::Right if self.right == RightView::Graph => self.graph.as_ref().map_or(0, |g| g.nbr),
            Pane::Right => self.annot_sel,
        };
        self.move_to(cur.saturating_add_signed(delta));
    }

    fn move_to(&mut self, i: usize) {
        match self.pane {
            Pane::Sidebar => {
                self.side_sel = i.min(self.side_visible.len() - 1);
                self.refilter();
            }
            Pane::List => {
                self.sel = i.min(self.visible.len().saturating_sub(1));
                self.extend_visual();
                self.follow_in_graph();
            }
            Pane::Right if self.right == RightView::Graph => {
                if let Some(g) = &mut self.graph {
                    g.nbr = i.min(g.neighbours().len().saturating_sub(1));
                }
            }
            Pane::Right if self.right == RightView::Tables => self.table_scroll = i.min(u16::MAX as usize) as u16,
            Pane::Right => self.annot_sel = i.min(self.annots.len().saturating_sub(1)),
        }
    }

    fn open_pdf(&mut self) {
        let Some(r) = self.selected() else { return };
        match self.lib.pdf_path(r) {
            Ok(p) if !p.exists() => {
                let Some(remote) = self.lib.remote_pdf(r) else {
                    self.status = format!("missing: {}", p.display());
                    return;
                };
                let account = yeda_secrets::webdav_account(&remote.dav.user, &remote.dav.url);
                let label = format!("{}@{}", remote.dav.user, remote.dav.url);
                let file = remote.file.clone();
                if let Some(password) = self.secret(&account, &label, Retry::OpenPdf) {
                    self.status = format!("downloading {file}…");
                    self.job = Some(Job::Download(password));
                }
            }
            Ok(p) => {
                let mut words = self.lib.reader.as_deref().unwrap_or_default().split_whitespace();
                let mut command = match words.next() {
                    Some(program) => Command::new(program),
                    None => {
                        let mut c = Command::new(std::env::current_exe().unwrap_or_else(|_| "yeda".into()));
                        c.arg("read");
                        c
                    }
                };
                let result = command
                    .args(words)
                    .arg(&p)
                    .arg("--annotations")
                    .arg(self.path(&library::annotations_dir(&r.key)))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if let Err(e) = result {
                    self.status = format!("{}: {e}", command.get_program().to_string_lossy());
                }
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    fn delete_selected(&mut self) {
        let effects = self.targets().iter().flat_map(|&i| self.lib.delete(&self.lib.refs[i].key)).collect();
        self.apply(Ok(effects));
        self.marked.clear();
        self.reload();
    }
}

impl App {
    fn in_graph(&self) -> bool {
        self.pane == Pane::Right && self.right == RightView::Graph && self.graph.is_some()
    }

    /// The OpenAlex key from the secret store, looked up once per session.
    fn openalex_key(&mut self) -> Option<String> {
        let key = self
            .secrets
            .entry(OPENALEX_ACCOUNT.into())
            .or_insert_with(|| yeda_secrets::get(OPENALEX_ACCOUNT).ok().flatten().unwrap_or_default());
        Some(key.clone()).filter(|k| !k.is_empty())
    }

    /// `args` is `selected` (the default), `filtered` or `prefetch`; `refresh` fetches citing works again.
    fn graph_command(&mut self, args: &str, refresh: bool, command: &str) {
        if self.graph_job.is_some() {
            self.status = "a graph is already being fetched".into();
            return;
        }
        let (scope, seeds): (&str, Vec<usize>) = match args.trim() {
            "key" => {
                let label = "OpenAlex API key (free at openalex.org/settings/api)".into();
                self.asking = Some(PendingSecret { account: OPENALEX_ACCOUNT.into(), label, retry: Retry::Command(String::new()) });
                self.input.clear();
                self.mode = Mode::Prompt(Prompt::Secret);
                return;
            }
            "" | "selected" => ("the selection", self.targets()),
            "filtered" => ("the list", self.visible.clone()),
            "prefetch" => ("the library", (0..self.lib.refs.len()).collect()),
            _ => {
                self.status = "usage: :graph[!] [selected|filtered|prefetch|key]".into();
                return;
            }
        };
        if seeds.is_empty() {
            self.status = "no reference selected".into();
            return;
        }
        let key = std::env::var("OPENALEX_API_KEY").ok().filter(|k| !k.is_empty()).or_else(|| self.openalex_key());
        let keyless = key.is_none();
        let rx = citations::spawn(citations::Request {
            refs: self.lib.refs.clone(),
            seeds: seeds.iter().map(|&i| self.lib.refs[i].key.clone()).collect(),
            settings: self.lib.graph.clone(),
            cache: self.lib.cache.clone().unwrap_or_else(std::env::temp_dir),
            key,
            refresh,
        });
        self.status = "graph: starting…".into();
        self.graph_job = Some(GraphJob { rx, command: command.into(), scope: scope.into(), keyless });
    }

    /// Applies a finished graph fetch; called from the event loop.
    pub fn poll_graph(&mut self) {
        let Some(job) = &self.graph_job else { return };
        loop {
            match job.rx.try_recv() {
                Ok(citations::Msg::Progress(s)) => self.status = format!("graph: {s}"),
                Ok(citations::Msg::Done(result)) => {
                    let job = self.graph_job.take().unwrap();
                    return self.finish_graph(result, job.scope);
                }
                Ok(citations::Msg::OverBudget) => {
                    let job = self.graph_job.take().unwrap();
                    if !job.keyless {
                        self.status = "OpenAlex's daily budget for this key is used up; what was fetched is cached".into();
                        return;
                    }
                    self.secrets.remove(OPENALEX_ACCOUNT);
                    let label = "OpenAlex API key (free at openalex.org/settings/api; the keyless daily budget is used up)";
                    if let Some(k) = self.secret(OPENALEX_ACCOUNT, label, Retry::Command(job.command.clone())) {
                        self.secrets.insert(OPENALEX_ACCOUNT.into(), k);
                        self.submit(Prompt::Command, &job.command);
                    }
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.graph_job = None;
                    return;
                }
            }
        }
    }

    fn finish_graph(&mut self, result: Result<citations::Fetched, String>, scope: String) {
        let f = match result {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("graph: {e}");
                return;
            }
        };
        let mut effects = Vec::new();
        for (key, id) in &f.matched {
            if let Some(i) = self.lib.refs.iter().position(|r| &r.key == key) {
                graph::set_work_id(&mut self.lib.refs[i], id);
                effects.push(self.lib.save(&self.lib.refs[i]));
            }
        }
        self.apply(effects.into_iter().collect());
        let unknown = match f.unknown.len() {
            0 => String::new(),
            n => format!(" · {n} not in OpenAlex (no DOI, or an unknown one)"),
        };
        if f.graph.nodes.is_empty() {
            self.status = format!("graph: nothing to show{unknown}");
            return;
        }
        self.status = format!("graph of {scope}: {} papers, {} citations{unknown}", f.graph.nodes.len(), f.graph.edges.len());
        let sel = self.selected().and_then(|r| f.graph.find_key(&r.key)).unwrap_or(0);
        self.graph = Some(GraphView { graph: f.graph, sel, nbr: 0, scope, back: Vec::new() });
        self.right = RightView::Graph;
    }

    /// Selects the list's reference in the graph, if it is there.
    fn follow_in_graph(&mut self) {
        let Some(key) = self.selected().map(|r| r.key.clone()) else { return };
        if let Some(g) = &mut self.graph
            && let Some(i) = g.graph.find_key(&key)
            && i != g.sel
        {
            g.sel = i;
            g.nbr = 0;
        }
    }

    fn graph_select(&mut self, i: usize) {
        let Some(g) = &mut self.graph else { return };
        g.sel = i;
        g.nbr = 0;
        // Move the list too, so the details and other commands follow the graph.
        if let Some(key) = g.graph.nodes[i].key.clone()
            && let Some(pos) = self.visible.iter().position(|&r| self.lib.refs[r].key == key)
        {
            self.sel = pos;
        }
    }

    fn graph_go(&mut self) {
        let Some(g) = &mut self.graph else { return };
        let Some(&(_, target)) = g.neighbours().get(g.nbr) else { return };
        g.back.push(g.sel);
        self.graph_select(target);
    }

    fn graph_back(&mut self) {
        if let Some(prev) = self.graph.as_mut().and_then(|g| g.back.pop()) {
            self.graph_select(prev);
        }
    }

    /// The highlighted neighbour, else the selected node.
    fn graph_target(&self) -> Option<&graph::Node> {
        let g = self.graph.as_ref()?;
        let i = g.neighbours().get(g.nbr).map_or(g.sel, |&(_, i)| i);
        g.graph.nodes.get(i)
    }

    fn graph_add(&mut self) {
        match self.graph_target().map(|n| (n.key.clone(), n.doi.clone())) {
            Some((Some(key), _)) => self.status = format!("{key} is already in the library"),
            Some((None, Some(doi))) => self.submit(Prompt::Add, &doi),
            _ => self.status = "this work has no DOI to add it by".into(),
        }
    }

    fn graph_open(&mut self) {
        let Some(doi) = self.graph_target().and_then(|n| n.doi.clone()) else {
            self.status = "this work has no DOI".into();
            return;
        };
        // ponytail: xdg-open only; macOS would need `open`.
        let result = Command::new("xdg-open").arg(format!("https://doi.org/{doi}")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Err(e) = result {
            self.status = format!("xdg-open: {e}");
        }
    }
}

impl App {
    /// `args` names the location, else `[fulltext] location`, else the only one there is.
    fn fetch_command(&mut self, args: &str, command: &str) {
        if self.fetch_job.is_some() {
            self.status = "a fetch is already running".into();
            return;
        }
        let names: Vec<String> = self.lib.locations.keys().chain(self.lib.remotes.keys()).cloned().collect();
        let location = match (args, &self.lib.fulltext.location, names.as_slice()) {
            ("", Some(l), _) => l.clone(),
            ("", None, [only]) => only.clone(),
            ("", None, _) => {
                self.status = format!("name a location (:fetch <location>, or [fulltext] location in yeda.toml): {}", names.join(", "));
                return;
            }
            (l, _, _) => l.to_string(),
        };
        if !names.contains(&location) {
            self.status = format!("no location named '{location}' (locations: {})", names.join(", "));
            return;
        }
        let password = match self.lib.remotes.get(&location) {
            Some(dav) => {
                let account = yeda_secrets::webdav_account(&dav.user, &dav.url);
                let label = format!("{}@{}", dav.user, dav.url);
                match self.secret(&account, &label, Retry::Command(command.into())) {
                    Some(p) => Some(p),
                    None => return,
                }
            }
            None => None,
        };
        let targets = self.targets();
        let items: Vec<fetch::Item> = targets
            .iter()
            .map(|&i| &self.lib.refs[i])
            .filter(|r| r.pdf.is_none())
            .map(|r| fetch::Item { key: r.key.clone(), doi: graph::lookup_doi(r), file: fulltext::file_name(r) })
            .collect();
        if items.is_empty() {
            self.status = if targets.is_empty() { "no reference selected" } else { "they already have PDFs" }.into();
            return;
        }
        let downloads = match (&self.lib.fulltext.downloads, dirs::home_dir()) {
            (Some(d), Some(home)) if d.starts_with("~") => home.join(d.strip_prefix("~").unwrap()),
            (Some(d), _) => d.clone(),
            (None, _) => dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("yeda/downloads"),
        };
        let openalex_key = std::env::var("OPENALEX_API_KEY").ok().filter(|k| !k.is_empty()).or_else(|| self.openalex_key());
        let request = fetch::Request { items, settings: self.lib.fulltext.clone(), downloads, openalex_key };
        self.status = "fetch: starting…".into();
        self.fetch_job = Some(FetchJob { rx: fetch::spawn(request), location, password, added: vec![], failed: vec![] });
    }

    /// Attaches fetched PDFs as they arrive; called from the event loop.
    pub fn poll_fetch(&mut self) {
        let Some(mut job) = self.fetch_job.take() else { return };
        loop {
            match job.rx.try_recv() {
                Ok(fetch::Msg::Progress(s)) => self.status = s,
                Ok(fetch::Msg::Saved { key, file, verify }) => match self.attach_fetched(&key, &file, verify, &job) {
                    Ok(()) => job.added.push(key),
                    Err(e) => job.failed.push((key, format!("{e:#}"))),
                },
                Ok(fetch::Msg::Failed { key, reason }) => job.failed.push((key, reason)),
                Ok(fetch::Msg::Done) | Err(TryRecvError::Disconnected) => return self.finish_fetch(job),
                Err(TryRecvError::Empty) => break,
            }
        }
        self.fetch_job = Some(job);
    }

    fn attach_fetched(&mut self, key: &str, file: &std::path::Path, verify: bool, job: &FetchJob) -> anyhow::Result<()> {
        let i = self.lib.refs.iter().position(|r| r.key == key).ok_or_else(|| anyhow::anyhow!("reference deleted meanwhile"))?;
        if verify {
            let text = poppler::pdf_text(file).unwrap_or_default();
            anyhow::ensure!(
                fulltext::looks_like(&text, &self.lib.refs[i]),
                "the downloaded PDF doesn't look like this paper (no DOI or title match); it's in {}",
                file.display()
            );
        }
        let r = self.attach_file(i, file, &job.location, job.password.as_deref())?;
        yeda_fs::apply(&self.dir, [self.lib.save(&r)?])?;
        self.lib.refs[i] = r;
        // Only once it is stored, so a failed upload leaves the download to retry with.
        let _ = fs::remove_file(file);
        Ok(())
    }

    fn finish_fetch(&mut self, job: FetchJob) {
        let mut parts = Vec::new();
        if !job.added.is_empty() {
            parts.push(format!("attached {} PDF{} to {}", job.added.len(), if job.added.len() == 1 { "" } else { "s" }, job.location));
        }
        if !job.failed.is_empty() {
            parts.push(self.report_failures(&job.failed));
        }
        self.status = if parts.is_empty() { "fetch: nothing fetched".into() } else { parts.join(" · ") };
        self.reload();
        if !job.failed.is_empty() {
            self.open_help(HelpPage::Failed);
        }
    }
}

impl App {
    /// Copies the targets as `format`, else `[cite] format`, else BibTeX.
    fn cite(&mut self, format: &str) {
        let refs: Vec<Ref> = self.targets().iter().map(|&i| self.lib.refs[i].clone()).collect();
        if refs.is_empty() {
            self.status = "no reference selected".into();
            return;
        }
        let format = if format.is_empty() { self.lib.cite.format.clone().unwrap_or_else(|| "bibtex".into()) } else { format.to_string() };
        let read = |path: &str| {
            let path = match (path.strip_prefix("~/"), dirs::home_dir()) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => self.dir.join(path),
            };
            fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
        };
        let text = match cite::export(&refs, &format, read) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("{e:#}");
                return;
            }
        };
        self.status = match self.copy(&text) {
            Ok(()) => format!("copied {} reference{} as {format}", refs.len(), if refs.len() == 1 { "" } else { "s" }),
            Err(e) => format!("clipboard: {e}"),
        };
    }
}

fn plural_suffix(n: usize) -> String {
    if n == 1 { String::new() } else { format!(" ({n} references)") }
}
