use crate::{citations, fetch, poppler, undo};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use crate::keys::{self, Keymap, Lookup};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::collections::{BTreeSet, HashMap, HashSet};
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
use yeda_core::library::{self, Library, Pdf, Ref, config_section};
use yeda_core::{cite, fulltext};
use yeda_core::graph::{self, Graph};
use yeda_core::search::{self, FILTER_FIELDS, Searcher};
use crate::cluster::ClusterView;
use crate::complete::Completion;
use crate::discover::Candidate;
use crate::setup::{self, SetupForm};
use crate::table::{self, TableView};
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
    /// Asking y/n before `App::confirming` runs.
    Confirm,
    /// The `:secrets` menu.
    Secrets,
    Filter,
    /// The `:setup` form.
    Setup,
    /// The papers `:discover` found.
    Found,
    /// git's output from `:sync` or `:git-init`.
    Git,
    Sort,
    /// Picking the table a command runs on.
    TablePick,
    /// Picking which of a reference's PDFs a command uses.
    PdfPick,
    /// The command history, to pick one to edit.
    History,
    Help(HelpPage),
}

#[derive(Clone, Copy, PartialEq)]
pub enum HelpPage {
    /// The key bindings and commands, from `?` or `:keys`.
    Keys,
    /// `GUIDE`, from `:help`.
    Guide,
    /// `App::report`: the identifiers the last magic-wand batch could not add, with why.
    Failed,
}

pub const GUIDE: &str = include_str!("guide.txt");

/// Every key and command, as `:help` lists them.
/// The actions keys are bound to, also run as `:<name>`.
pub const ACTIONS: &[(&str, &str)] = &[
    ("down", "move down"),
    ("up", "move up"),
    ("half-down", "move half a page down"),
    ("half-up", "move half a page up"),
    ("top", "go to the first entry"),
    ("bottom", "go to the last entry"),
    ("left", "focus the column to the left"),
    ("right", "focus the column to the right"),
    ("next-column", "focus the next column"),
    ("confirm", "end a range (keeping its marks), else focus the column to the right"),
    ("column", "show only the current column; again to show all"),
    ("search", "search; in the library column, filter it (:help explains the syntax)"),
    ("filter", "filter by field, with suggestions from the library"),
    ("sort", "sort"),
    ("clear", "cancel a range, else unmark all, else clear the search"),
    ("add", "add by DOI, arXiv ID or ISBN, into a collection you pick; a pasted list adds each"),
    ("open", "open the PDF in the reader (yeda read)"),
    ("edit", "edit the reference file in $EDITOR; on a saved search in the library column, its name, query and sort"),
    ("note", "edit the reference's note in $EDITOR"),
    ("duplicate", "on a saved search in the library column, a new one filled in from it; saved on Enter"),
    ("mark", "mark or unmark the reference"),
    ("range", "mark a range as you move"),
    ("tags", "edit tags"),
    ("move", "move to a collection"),
    ("status-next", "next reading status"),
    ("status-prev", "previous reading status"),
    ("undo", "undo the last command's changes to the library"),
    ("redo", "redo what undo took back"),
    ("annotations", "show annotations"),
    ("extract", "extract the annotations embedded in the PDF"),
    ("tag-color", "colour the selected tag (library column)"),
    ("delete", "delete the reference or saved search"),
    ("command", "command line; Up/Down recall earlier commands, Tab/shift-Tab complete"),
    ("close-or-quit", "close the open table, graph or annotations, else quit"),
    ("graph-go", "go to the picked neighbour"),
    ("graph-back", "go back"),
    ("graph-tab", "next list under the graph: seeds, cites, cited by, top cited, top citing"),
    ("graph-tab-back", "previous list under the graph"),
    ("graph-sort", "sort the lists by global citations or by year"),
    ("graph-add", "add the picked paper by DOI"),
    ("graph-open", "open the picked paper's DOI in the browser"),
    ("neighbourhood", "show only the selected paper and its direct links"),
    ("graph-scroll-down", "scroll the graph down"),
    ("graph-scroll-up", "scroll the graph up"),
    ("open-annotation", "open the image in the viewer, or the entry in $EDITOR"),
    ("cluster-go", "select the picked paper or label"),
    ("cluster-back", "go back"),
    ("cluster-tab", "list the selected node's links, or every node"),
    ("table-left", "previous column of the table; from the first, the reference list"),
    ("table-right", "next column of the table"),
    ("table-hide", "hide the column"),
    ("table-show", "show the hidden columns again"),
    ("table-cell", "show the cell in full, or close it"),
    ("table-edit", "edit the cell in $EDITOR"),
    ("table-sort", "sort by the column; again reverses"),
    ("table-search", "show only rows with every word in some cell"),
    ("table-filter", "show only rows whose cell in this column matches (!x: doesn't; \"\": empty)"),
    ("table-clear", "close the cell, else clear search and filters, else close the table"),
];

/// Actions that change every marked reference, asked about first: action, and what it does.
const CONFIRM_MANY: [(&str, &str); 5] = [
    ("status-next", "change the reading status of"),
    ("status-prev", "change the reading status of"),
    ("tags", "change the tags of"),
    ("move", "move"),
    ("extract", "extract annotations into"),
];

pub const COMMANDS: &[(&str, &str)] = &[
    (":save <name>", "save the current search"),
    (":summarize[!] [table] [profile]", "fill the reference's table row with an LLM; ! overwrites"),
    (":cancel", "stop summarizing after the current paper"),
    (":export <table> <file.csv>", "export a table"),
    (":attach <file.pdf> [location]", "add a PDF, copying or uploading it to a location"),
    (":detach", "remove one of the selected reference's PDFs with its annotations; the file stays where it is"),
    (":sync", "commit, pull and push the library with git"),
    (":git-init [url]", "make the library folder a git repository and commit it; with a URL, push it there as origin"),
    (":graph[!] [citations]", "citation graph of the selected or marked references, from OpenAlex (:help explains it)"),
    (":graph tags", "the selected or marked references and their tags, clustered"),
    (":graph col <column>", "the same with a table column's values, such as :graph col group"),
    (":discover [what]", "ask the LLM (searching the web where it can) for papers like the selected or marked ones, or as described; a adds one"),
    (":found", "the papers the last :discover found"),
    (":history", "past commands; Enter puts one on the command line (Ctrl-R there)"),
    (":setup llm|webdav [name]", "add or change an LLM profile or WebDAV location in yeda.toml, testing it first"),
    (":setup openalex", "set the optional OpenAlex API key, testing it first"),
    (":secrets", "every password and API key the config uses: where each is set; set, replace or remove them"),
    (":graph close", "close the graph shown"),
    (":table [name]", "a table of the selected or marked references; without a name, pick one (:help explains it)"),
    (":table close", "close the table"),
    (":table rename <old> <new>", "rename a table in yeda.toml and in every paper's LLM file"),
    (":cite [format]", "copy the selected or marked references: bibtex, ident (DOI, arXiv ID, ISBN or URL), a style (ieee, apa, …) or a .csl file"),
    (":fetch [location]", "find and attach PDFs for the selected or marked references (:help explains it)"),
    (":keys", "this list"),
    (":help", "guide: search syntax (AND, OR, NOT, fields), filters, saved searches"),
    (":failed", "what the last add or fetch could not get, with the reasons"),
    (":log", "every status message and failure, with times, in $EDITOR"),
    (":q", "quit"),
    (":<action>", "any action above, such as :annotations"),
];

/// Editing keys of the command line and every other text box; fixed, not in the keymap.
pub const TEXT_KEYS: &[(&str, &str)] = &[
    ("Left Right Home End", "move the cursor (Ctrl-A Ctrl-E: start / end)"),
    ("Backspace Delete", "delete before / at the cursor"),
    ("Ctrl-W Ctrl-U", "delete the word / everything before the cursor"),
    ("Ctrl-G", "edit the text in $EDITOR (on the command line also Ctrl-X Ctrl-E)"),
    ("Tab Shift-Tab", "on the command line: complete the last word"),
    ("Up Down", "on the command line: earlier / later commands"),
    ("Ctrl-R", "on the command line: past commands containing what is typed (:history)"),
];

pub const NOTES: &[(&str, &str)] = &[
    ("mouse", "a click selects and focuses; the wheel moves the selection, or scrolls the graph"),
    ("narrow", "below 120 columns the right column is hidden, below 80 the library column too"),
    ("keys", "change them under [keys.<context>] in ~/.config/yeda/config.toml (:help shows how)"),
];

/// The `f` popup: one value per `search::FILTER_FIELDS` entry, then free text.
/// A `:setup` connection test running; saved when it passes.
struct SetupJob {
    rx: Receiver<anyhow::Result<()>>,
    section: toml::Table,
    setup: setup::Setup,
    secret: Option<String>,
}

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
    /// Words the table's rows must match, applied as typed.
    TableSearch,
    /// What the table's current column must match, applied as typed.
    TableFilter,
}

/// Applies an editing key to `buf` with its cursor `back` characters from the end; false for
/// keys it doesn't handle.
fn edit_line(buf: &mut String, back: &mut usize, code: KeyCode, ctrl: bool) -> bool {
    let len = buf.chars().count();
    *back = (*back).min(len);
    let at = buf.char_indices().nth(len - *back).map_or(buf.len(), |(i, _)| i);
    match code {
        KeyCode::Char('u') if ctrl => buf.replace_range(..at, ""),
        KeyCode::Char('w') if ctrl => {
            let keep = buf[..at].trim_end().rfind(' ').map_or(0, |i| i + 1);
            buf.replace_range(keep..at, "");
        }
        KeyCode::Char('a') if ctrl => *back = len,
        KeyCode::Char('e') if ctrl => *back = 0,
        KeyCode::Char(_) if ctrl => {}
        KeyCode::Char(c) => buf.insert(at, c),
        KeyCode::Backspace => {
            if let Some(c) = buf[..at].chars().next_back() {
                buf.replace_range(at - c.len_utf8()..at, "");
            }
        }
        KeyCode::Delete => {
            if *back > 0 {
                buf.remove(at);
                *back -= 1;
            }
        }
        KeyCode::Left => *back = (*back + 1).min(len),
        KeyCode::Right => *back = back.saturating_sub(1),
        KeyCode::Home => *back = len,
        KeyCode::End => *back = 0,
        _ => return false,
    }
    true
}

/// `s` without one pair of surrounding quotes, as a path copied from a shell has.
fn unquote(s: &str) -> &str {
    ['"', '\''].iter().find_map(|q| s.strip_prefix(*q)?.strip_suffix(*q)).unwrap_or(s)
}

/// A typed text that $EDITOR can edit.
#[derive(Clone, Copy)]
pub enum TextTarget {
    /// The command line and the other prompts.
    Input,
    Query,
    SideQuery,
    Filter(usize),
    Setup(usize),
}

pub struct GitView {
    pub title: String,
    pub lines: Vec<String>,
    /// What came of it, once it is done.
    pub done: Option<String>,
    rx: Receiver<GitMsg>,
}

enum GitMsg {
    Line(String),
    Done(anyhow::Result<String>),
}

/// What to run again once a missing secret has been entered.
#[derive(Clone)]
pub enum Retry {
    OpenPdf,
    Command(String),
    /// Back to the `:secrets` menu.
    Secrets,
}

/// A secret the configuration can use, as `:secrets` lists it.
pub struct SecretItem {
    pub account: String,
    pub label: String,
    /// Where it is set, or "not set"; never the value.
    pub status: String,
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
    /// `:graph tags` or `:graph col`.
    Clusters,
}

const OPENALEX_ACCOUNT: &str = yeda_secrets::OPENALEX;

/// The citation graph shown in the right column; its selected node follows the reference list.
pub struct GraphView {
    pub graph: Graph,
    pub sel: usize,
    /// The highlighted entry of `entries()`.
    pub nbr: usize,
    pub scope: String,
    pub tab: GraphTab,
    /// Sort the per-paper tabs by year instead of global citation count.
    pub by_year: bool,
    /// Work ids navigated away from, for Backspace; ids, as a rebuild renumbers the nodes.
    back: Vec<String>,
    /// Show only the selected paper and its direct links.
    pub ego: bool,
    /// The first text row shown, and the node it was last moved to show.
    pub scroll: usize,
    pub scroll_for: Option<usize>,
    /// Library keys of the starting seeds, work ids of the papers opened with Enter, and of those
    /// among them that weren't seeds.
    seeds: Vec<String>,
    opened: Vec<String>,
    explored: Vec<String>,
}

/// What the list under the graph shows.
#[derive(Clone, Copy, PartialEq)]
pub enum GraphTab {
    Seeds,
    /// Works the selected paper cites.
    Cites,
    /// Works citing the selected paper.
    CitedBy,
    /// Works cited together with the selected paper.
    Cocited,
    /// Outside works cited by the most seeds.
    TopCited,
    /// Outside works citing the most seeds.
    TopCiting,
}

impl GraphTab {
    pub const ALL: [GraphTab; 6] = [GraphTab::Seeds, GraphTab::Cites, GraphTab::CitedBy, GraphTab::Cocited, GraphTab::TopCited, GraphTab::TopCiting];

    pub fn name(self) -> &'static str {
        match self {
            GraphTab::Seeds => "Seeds",
            GraphTab::Cites => "Cites →",
            GraphTab::CitedBy => "← Cited by",
            GraphTab::Cocited => "⇄ Co-cited",
            GraphTab::TopCited => "▲ Top cited",
            GraphTab::TopCiting => "▼ Top citing",
        }
    }
}

impl GraphView {
    /// The current tab's papers, as `(mark, node)`, by global citations or year; the top lists
    /// mark how many seeds each is cited by (▲) or cites (▼), co-cited papers how many works cite both (×).
    pub fn entries(&self) -> Vec<(String, usize)> {
        let nodes = &self.graph.nodes;
        let (citing, cited) = self.graph.neighbours(self.sel);
        let outside = |count: fn(&graph::Node) -> usize| (0..nodes.len()).filter(|&i| nodes[i].key.is_none() && count(&nodes[i]) > 0).map(|i| (i, count(&nodes[i]))).collect();
        let plain = |ids: Vec<usize>| ids.into_iter().map(|i| (i, 0)).collect();
        let (mut ids, mark): (Vec<(usize, usize)>, &str) = match self.tab {
            GraphTab::Seeds => (plain((0..nodes.len()).filter(|&i| nodes[i].seed).collect()), ""),
            GraphTab::Cites => (plain(cited), ""),
            GraphTab::CitedBy => (plain(citing), ""),
            GraphTab::Cocited => (nodes.get(self.sel).map(|n| n.cocited.clone()).unwrap_or_default(), "×"),
            GraphTab::TopCited => (outside(|n| n.cited_by_seeds), "▲"),
            GraphTab::TopCiting => (outside(|n| n.cites_seeds), "▼"),
        };
        if self.by_year {
            ids.sort_by_key(|&(i, _)| std::cmp::Reverse(nodes[i].year));
        } else {
            ids.sort_by_key(|&(i, _)| std::cmp::Reverse(nodes[i].cited_by));
        }
        ids.into_iter().map(|(i, n)| (if mark.is_empty() { String::new() } else { format!("{mark}{n}") }, i)).collect()
    }
}

struct FetchJob {
    rx: Receiver<fetch::Msg>,
    step: undo::Step,
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
    seeds: Vec<String>,
    /// For a rebuild after opening a paper: the paper to select.
    focus: Option<String>,
}

pub struct Batch {
    step: undo::Step,
    /// The collection new references go into.
    collection: Option<String>,
    todo: std::collections::VecDeque<String>,
    total: usize,
    added: Vec<String>,
    existed: Vec<String>,
    failed: Vec<(String, String)>,
}

pub enum Job {
    Fetch(Batch),
    ExtractAnnotations,
    /// Download one of the selected reference's PDFs from WebDAV with this password, then open it.
    Download(Pdf, String),
    Attach { file: PathBuf, location: String, password: Option<String> },
}

#[derive(Clone, Copy, PartialEq)]
pub enum PickKind {
    Tags,
    Move,
    /// The collection the identifiers in `App::pending_add` go into.
    AddTo,
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
    /// Whether Enter acts on the highlighted option; for tags, Enter on an empty input is done.
    pub fn active(&self, input: &str) -> bool {
        self.kind != PickKind::Tags || self.moved || !input.trim().is_empty()
    }
}

enum WorkerMsg {
    Started(String),
    Finished { key: String, result: Result<(Row, Option<String>), String> },
}

struct Summarizer {
    rx: Receiver<WorkerMsg>,
    step: undo::Step,
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
    /// What commands wrote, to undo; a `RefCell` so writes from `&self` are recorded too.
    undo: std::cell::RefCell<undo::History>,
    /// The command running now, which writes are recorded under.
    step: undo::Step,
    /// Secrets entered or read this session, by secret-store account.
    secrets: std::collections::HashMap<String, String>,
    /// Accounts whose stored secret the server rejected, so the store is skipped until a new one is entered.
    rejected: std::collections::HashSet<String>,
    pub secret_items: Vec<SecretItem>,
    pub secret_sel: usize,
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
    /// What the last frame drew where: lists with their scroll offset, then whole columns.
    pub clickable: Vec<(Pane, Rect, Option<usize>)>,
    /// Keys typed so far of a longer binding, such as the first `g` of `g g`.
    pending: Vec<String>,
    pub keymap: Keymap,
    pub status: String,
    pub quit: bool,
    /// A file for $EDITOR, and the line to start at.
    pub edit: Option<(PathBuf, Option<usize>)>,
    /// `:sync` or `:git-init`, running or done, with git's output.
    pub git: Option<GitView>,
    pub job: Option<Job>,
    picker: Picker,
    summarizer: Option<Summarizer>,
    pub chooser: Option<Chooser>,
    /// Identifiers to add once their collection is picked, and the mode to go back to.
    pending_add: Option<(String, Mode)>,
    pub right: RightView,
    pub table: Option<TableView>,
    pub cluster: Option<ClusterView>,
    /// The selected reference's annotations, on each of its PDFs.
    pub annots: Vec<(Pdf, Annotation)>,
    annots_key: Option<String>,
    annots_modified: Vec<Option<std::time::SystemTime>>,
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
    /// The filter `refilter` last showed; choosing a saved search applies its sort once.
    shown_filter: Filter,
    pub sort_sel: usize,
    pub form: Option<FilterForm>,
    pub setup: Option<SetupForm>,
    /// The action `Mode::Confirm` asks about, with its arguments.
    confirming: Option<(String, String)>,
    /// The next `action` call was confirmed already.
    confirmed: bool,
    discover_job: Option<Receiver<anyhow::Result<Vec<Candidate>>>>,
    pub found: Vec<Candidate>,
    pub found_sel: usize,
    setup_job: Option<SetupJob>,
    pub help_scroll: u16,
    /// What `/` searches for in a help page, and whether it's still being typed.
    pub help_query: String,
    pub help_typing: bool,
    /// The help page's text, as last drawn, for n/N.
    pub help_lines: Vec<String>,
    pub log: crate::log::Log,
    /// Earlier `:` commands, oldest first, and the one Up/Down is showing.
    history: Vec<String>,
    history_at: Option<usize>,
    /// The command line's cursor, in characters from the end.
    pub cursor_back: usize,
    /// Ctrl-X was pressed on the command line, starting Ctrl-X Ctrl-E.
    ctrl_x: bool,
    /// The file a typed text is being edited in by $EDITOR, and where it goes back.
    pub edit_text: Option<(PathBuf, TextTarget)>,
    /// What typed text the history list shows commands containing.
    pub history_filter: String,
    pub history_sel: usize,
    /// Tab completion in progress on the command line.
    pub completion: Option<Completion>,
    /// The command waiting for a table from the picker, and the arguments after the table's name.
    table_pick: Option<(String, String)>,
    pub table_sel: usize,
    pub pdf_pick: Option<PdfPick>,
    pub pdf_sel: usize,
    /// The PDF picked for each reference, by key, while a command and its retries run.
    pdf_chosen: HashMap<String, String>,
}

/// A reference's PDFs to pick from, most annotated first, and what runs again with the one picked.
pub struct PdfPick {
    pub key: String,
    /// Each PDF with how many annotations it has.
    pub pdfs: Vec<(Pdf, usize)>,
    retry: Retry,
}

const HISTORY_LEN: usize = 200;

impl App {
    pub fn new(dir: PathBuf, lib: Library, picker: Picker, keymap: Keymap) -> Self {
        let history = lib.cache.as_ref().and_then(|c| fs::read_to_string(c.join("history")).ok()).unwrap_or_default();
        let status = unreadable(&lib);
        let mut app = Self {
            history: history.lines().map(String::from).collect(),
            history_at: None,
            cursor_back: 0,
            ctrl_x: false,
            edit_text: None,
            history_filter: String::new(),
            history_sel: 0,
            completion: None,
            table_pick: None,
            pdf_pick: None,
            pdf_sel: 0,
            pdf_chosen: HashMap::new(),
            table_sel: 0,
            log: crate::log::Log::open(),
            undo: Default::default(),
            step: undo::Step::default(),
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
            clickable: Vec::new(),
            pending: Vec::new(),
            keymap,
            status,
            quit: false,
            edit: None,
            git: None,
            job: None,
            picker,
            summarizer: None,
            chooser: None,
            pending_add: None,
            right: RightView::Details,
            table: None,
            cluster: None,
            annots: Vec::new(),
            annots_key: None,
            annots_modified: Vec::new(),
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
            shown_filter: Filter::All,
            sort_sel: 0,
            form: None,
            setup: None,
            confirming: None,
            confirmed: false,
            discover_job: None,
            found: Vec::new(),
            found_sel: 0,
            setup_job: None,
            help_scroll: 0,
            help_query: String::new(),
            help_typing: false,
            help_lines: Vec::new(),
            secrets: Default::default(),
            rejected: Default::default(),
            secret_items: Vec::new(),
            secret_sel: 0,
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
        if filter != self.shown_filter {
            if let Filter::Search(name, _) = &filter
                && let Some(order) = self.lib.searches.get(name).and_then(|s| s.order())
            {
                self.sort = order;
            }
            self.shown_filter = filter.clone();
        }
        self.visible = browse::visible(&mut self.searcher, &self.lib, &filter, &self.query);
        browse::sort(&self.lib, &mut self.visible, self.sort);
        // Commands act on marked references, so none may stay marked out of sight.
        let shown: HashSet<&str> = self.visible.iter().map(|&i| self.lib.refs[i].key.as_str()).collect();
        self.marked.retain(|k| shown.contains(k.as_str()));
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
        match effects.and_then(|e| self.write(&self.step, e)) {
            Ok(()) => true,
            Err(e) => {
                self.status = format!("{e:#}");
                false
            }
        }
    }

    /// Applies `effects`, recording them under `step` for undo.
    fn write(&self, step: &undo::Step, effects: impl IntoIterator<Item = Effect>) -> anyhow::Result<()> {
        self.undo.borrow_mut().apply(&self.dir, step, effects)
    }

    fn undo_or_redo(&mut self, redo: bool) {
        let result = {
            let mut history = self.undo.borrow_mut();
            if redo { history.redo(&self.dir) } else { history.undo(&self.dir) }
        };
        self.status = match result {
            Ok((label, n)) => {
                self.reload();
                let what = if label.is_empty() { String::new() } else { format!(" :{label}") };
                format!("{}{what} ({n} file{})", if redo { "redid" } else { "undid" }, if n == 1 { "" } else { "s" })
            }
            Err(e) => format!("{e:#}"),
        };
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn reload(&mut self) {
        match crate::load_library(&self.dir) {
            Ok(lib) => {
                if !lib.unreadable.is_empty() {
                    self.status = unreadable(&lib);
                }
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
        // Each key press is a new command for undo, unless it continues one (a confirmation, a pick).
        if matches!(self.mode, Mode::Normal | Mode::Prompt(Prompt::Command)) {
            self.step = undo::Step { id: self.step.id + 1, label: String::new() };
        }
        // A key here starts a new command, so earlier picks no longer apply.
        if matches!(self.mode, Mode::Normal | Mode::Prompt(Prompt::Command)) {
            self.pdf_chosen.clear();
        }
        let mode = self.mode;
        let field = (self.form.as_ref().map(|f| f.sel), self.setup.as_ref().map(|f| f.sel));
        self.on_key_in_mode(k);
        if self.mode != mode || field != (self.form.as_ref().map(|f| f.sel), self.setup.as_ref().map(|f| f.sel)) {
            self.cursor_back = 0;
        }
    }

    fn on_key_in_mode(&mut self, k: KeyEvent) {
        match self.mode {
            Mode::Normal => self.normal_key(k),
            Mode::Secrets => self.secrets_key(k),
            Mode::Confirm => {
                self.mode = Mode::Normal;
                let pending = self.confirming.take();
                match pending {
                    Some((action, _)) if k.code == KeyCode::Char('y') && action == "delete" => match self.current_filter().clone() {
                        Filter::Search(name, _) if self.pane == Pane::Sidebar => self.delete_search(&name),
                        _ => self.delete_selected(),
                    },
                    Some((action, command)) if k.code == KeyCode::Char('y') && action == "command" => {
                        self.confirmed = true;
                        self.submit(Prompt::Command, &command);
                        self.confirmed = false;
                    }
                    Some((action, args)) if k.code == KeyCode::Char('y') => {
                        self.confirmed = true;
                        self.action(&action, &args);
                        self.confirmed = false;
                    }
                    _ => self.status = "cancelled".into(),
                }
            }
            Mode::Pick => self.pick_key(k),
            Mode::Filter => self.filter_key(k),
            Mode::Setup => self.setup_key(k),
            Mode::Found => self.found_key(k),
            Mode::Git => self.git_key(k),
            Mode::Sort => self.sort_key(k),
            Mode::TablePick => self.table_pick_key(k),
            Mode::PdfPick => self.pdf_pick_key(k),
            Mode::History => self.history_key(k),
            Mode::Help(_) => self.help_key(k),
            Mode::Search | Mode::SideSearch | Mode::Prompt(_) => self.input_key(k),
        }
    }

    fn normal_key(&mut self, k: KeyEvent) {
        let Some(key) = keys::name(&k) else { return };
        let mut seq = std::mem::take(&mut self.pending);
        seq.push(key.clone());
        let context = self.context();
        match self.keymap.lookup(context, &seq) {
            Lookup::Run(command) => self.submit(Prompt::Command, &command),
            Lookup::Prefix => self.pending = seq,
            // Like vim: a key that doesn't continue a sequence starts over on its own.
            Lookup::None if seq.len() > 1 => match self.keymap.lookup(context, std::slice::from_ref(&key)) {
                Lookup::Run(command) => self.submit(Prompt::Command, &command),
                Lookup::Prefix => self.pending = vec![key],
                Lookup::None => {}
            },
            Lookup::None => {}
        }
    }

    /// The keymap context of the focused column.
    fn context(&self) -> &'static str {
        match self.pane {
            Pane::Sidebar => "library",
            Pane::Right if self.in_graph() => "graph",
            Pane::Right if self.right == RightView::Annotations => "annotations",
            Pane::Right if self.right == RightView::Tables && self.table.is_some() => "table",
            Pane::Right if self.right == RightView::Clusters && self.cluster.is_some() => "cluster",
            _ => "normal",
        }
    }

    /// Runs the action `name` from `ACTIONS`; false if there is none.
    fn action(&mut self, name: &str, args: &str) -> bool {
        let n = self.targets().len();
        if !std::mem::take(&mut self.confirmed)
            && n > 1
            && let Some((_, what)) = CONFIRM_MANY.iter().find(|(a, _)| *a == name)
        {
            self.status = format!("{what} {n} marked references? (y/n)");
            self.confirming = Some((name.into(), args.into()));
            self.mode = Mode::Confirm;
            return true;
        }
        let half = (self.page / 2).max(1) as isize;
        match name {
            "down" => self.step(1),
            "up" => self.step(-1),
            "half-down" => self.step(half),
            "half-up" => self.step(-half),
            "top" => self.move_to(0),
            "bottom" => self.move_to(usize::MAX),
            "graph-scroll-down" | "graph-scroll-up" => {
                if let Some(g) = &mut self.graph {
                    g.scroll = if name == "graph-scroll-down" { g.scroll + 1 } else { g.scroll.saturating_sub(1) };
                }
            }
            "delete" => match (self.pane, self.current_filter()) {
                (Pane::Sidebar, Filter::Search(name, _)) => {
                    self.status = format!("delete saved search '{name}'? (y/n)");
                    self.confirming = Some(("delete".into(), String::new()));
                    self.mode = Mode::Confirm;
                }
                (Pane::List, _) => match self.targets()[..] {
                    [] => {}
                    [i] => {
                        self.status = format!("delete {}? (y/n)", self.lib.refs[i].key);
                        self.confirming = Some(("delete".into(), String::new()));
                    self.mode = Mode::Confirm;
                    }
                    ref many => {
                        self.status = format!("delete {} marked references? (y/n)", many.len());
                        self.confirming = Some(("delete".into(), String::new()));
                    self.mode = Mode::Confirm;
                    }
                },
                _ => {}
            },
            "left" => self.pane = if self.pane == Pane::Right { Pane::List } else { Pane::Sidebar },
            "right" => {
                let right_focusable = self.right != RightView::Details;
                self.pane = if self.pane == Pane::Sidebar || !right_focusable { Pane::List } else { Pane::Right }
            }
            "confirm" if self.visual.is_some() => self.visual = None,
            "confirm" => return self.action("right", ""),
            "next-column" => {
                self.pane = match self.pane {
                    Pane::Sidebar => Pane::List,
                    Pane::List if self.right != RightView::Details => Pane::Right,
                    _ => Pane::Sidebar,
                }
            }
            "graph-go" => self.graph_go(),
            "graph-back" => self.graph_back(),
            "graph-tab" | "graph-tab-back" => {
                if let Some(g) = &mut self.graph {
                    let at = GraphTab::ALL.iter().position(|t| *t == g.tab).unwrap_or(0);
                    let step = if name == "graph-tab" { 1 } else { GraphTab::ALL.len() - 1 };
                    g.tab = GraphTab::ALL[(at + step) % GraphTab::ALL.len()];
                    g.nbr = 0;
                }
            }
            "graph-sort" => {
                if let Some(g) = &mut self.graph {
                    g.by_year = !g.by_year;
                    g.nbr = 0;
                }
            }
            "graph-add" => self.graph_add(),
            "graph-open" => self.graph_open(),
            "neighbourhood" => {
                if let Some(g) = &mut self.graph {
                    g.ego = !g.ego;
                    g.scroll_for = None;
                }
            }
            "open-annotation" => self.open_annotation(),
            "annotations" => self.toggle_right(RightView::Annotations),
"cluster-go" => {
                if let Some(c) = &mut self.cluster
                    && let Some(&i) = c.entries().get(c.nbr)
                {
                    c.go(i);
                    self.cluster_follow_list();
                }
            }
            "cluster-back" => {
                if let Some(c) = &mut self.cluster
                    && let Some(i) = c.back.pop()
                {
                    c.sel = i;
                    c.nbr = 0;
                    self.cluster_follow_list();
                }
            }
            "cluster-tab" => {
                if let Some(c) = &mut self.cluster {
                    c.all = !c.all;
                    c.nbr = 0;
                }
            }
            "table-left" => {
                let n = self.table.as_ref().map_or(0, |t| table::columns(&self.lib, &t.name).len());
                let prev = self.table.as_ref().and_then(|t| t.visible(n).into_iter().rfind(|&c| c < t.col));
                match (&mut self.table, prev) {
                    (Some(t), Some(c)) => t.col = c,
                    _ => self.pane = Pane::List,
                }
            }
            "table-right" => {
                let n = self.table.as_ref().map_or(0, |t| table::columns(&self.lib, &t.name).len());
                if let Some(t) = &mut self.table
                    && let Some(c) = t.visible(n).into_iter().find(|&c| c > t.col)
                {
                    t.col = c;
                }
            }
            "table-hide" => {
                let n = self.table.as_ref().map_or(0, |t| table::columns(&self.lib, &t.name).len());
                if let Some(t) = &mut self.table {
                    t.hide(n);
                }
            }
            "table-show" => {
                if let Some(t) = &mut self.table {
                    t.hidden.clear();
                }
            }
            "table-cell" => {
                if let Some(t) = &mut self.table {
                    t.open = !t.open;
                }
            }
            "table-sort" => {
                if let Some(t) = &mut self.table {
                    t.sort = match t.sort {
                        Some((c, false)) if c == t.col => Some((c, true)),
                        _ => Some((t.col, false)),
                    };
                }
            }
            "table-search" => {
                self.input = self.table.as_ref().map(|t| t.query.clone()).unwrap_or_default();
                self.mode = Mode::Prompt(Prompt::TableSearch);
            }
            "table-filter" => {
                let Some(t) = &self.table else { return true };
                self.input = t.filters.iter().find(|(c, _)| *c == t.col).map(|(_, v)| v.clone()).unwrap_or_default();
                self.mode = Mode::Prompt(Prompt::TableFilter);
            }
            "table-edit" => self.table_edit(),
            "table-clear" => match &mut self.table {
                Some(t) if t.open => t.open = false,
                Some(t) if !t.query.is_empty() || !t.filters.is_empty() => {
                    t.query.clear();
                    t.filters.clear();
                }
                _ => self.close_table(),
            },
            "extract" if !self.targets().is_empty() => {
                self.status = "extracting annotations…".into();
                self.job = Some(Job::ExtractAnnotations);
            }
            "extract" => {}
            "search" if self.pane == Pane::Sidebar => self.mode = Mode::SideSearch,
            "search" => {
                self.pane = Pane::List;
                self.mode = Mode::Search;
            }
            "mark" if self.pane == Pane::List => self.toggle_mark(),
            "range" if self.pane == Pane::List => {
                self.visual = match self.visual {
                    Some(_) => None,
                    None => Some((self.sel, self.marked.clone())),
                };
                self.extend_visual();
            }
            "mark" | "range" => {}
            "clear" => {
                if let Some((_, before)) = self.visual.take() {
                    self.marked = before;
                } else if self.pane == Pane::List && !self.marked.is_empty() {
                    self.status = format!("unmarked {}", self.marked.len());
                    self.marked.clear();
                } else if self.pane == Pane::Sidebar {
                    self.side_query.clear();
                    self.refilter_sidebar(false);
                } else {
                    self.query.clear();
                    self.refilter();
                }
            }
            "status-next" => self.cycle_status(true),
            "status-prev" => self.cycle_status(false),
            "undo" => self.undo_or_redo(false),
            "redo" => self.undo_or_redo(true),
            "column" => self.focus = !self.focus,
            "filter" => self.open_filter(),
            "sort" => {
                self.sort_sel = SortField::ALL.iter().position(|f| *f == self.sort.field).unwrap_or(0);
                self.mode = Mode::Sort;
            }
            "tag-color" => match self.current_filter() {
                Filter::Tag(t) if self.pane == Pane::Sidebar => {
                    self.input = self.lib.tag_colors.get(t).cloned().unwrap_or_default();
                    self.mode = Mode::Prompt(Prompt::TagColor);
                }
                _ => self.status = "select a tag in the library pane to color it".into(),
            },
            "open" => self.open_pdf(),
            "edit" if self.pane == Pane::Sidebar && matches!(self.current_filter(), Filter::Search(..)) => {
                self.setup = Some(SetupForm::new(setup::Kind::Search, self.current_filter().name(), &self.lib));
                self.mode = Mode::Setup;
            }
            "edit" => self.edit = self.selected().map(|r| (self.path(&library::ref_path(&r.key)), None)),
            "duplicate" => match self.current_filter() {
                Filter::Search(name, _) if self.pane == Pane::Sidebar => {
                    self.setup = Some(SetupForm::duplicate(name, &self.lib));
                    self.mode = Mode::Setup;
                }
                _ => self.status = "select a saved search in the library column to duplicate it".into(),
            },
            "note" => self.edit = self.selected().map(|r| (self.path(&library::note_path(&r.key)), None)),
            "tags" => self.open_chooser(PickKind::Tags),
            "move" => self.open_chooser(PickKind::Move),
            "command" => self.mode = Mode::Prompt(Prompt::Command),
            "close-or-quit" if self.right == RightView::Tables && self.table.is_some() => self.close_table(),
            "close-or-quit" if self.right == RightView::Clusters && self.cluster.is_some() => self.close_cluster(),
            "close-or-quit" if self.graph.is_some() => self.close_graph(),
            "close-or-quit" if self.right == RightView::Annotations => self.back_to_details(),
            "close-or-quit" => self.quit = true,
            "add" if args.is_empty() => self.mode = Mode::Prompt(Prompt::Add),
            "add" => self.submit(Prompt::Add, args),
            _ => return false,
        }
        true
    }

    /// Types pasted text into the open prompt; ignored in modes where letters are commands.
    pub fn on_paste(&mut self, text: &str) {
        let add = matches!(self.mode, Mode::Prompt(Prompt::Add));
        if !matches!(self.mode, Mode::Prompt(_) | Mode::Search | Mode::SideSearch | Mode::Pick | Mode::Filter | Mode::Setup) {
            return;
        }
        // Lines separate identifiers in a pasted list; elsewhere a newline would only end the prompt.
        // Terminals differ in sending line breaks as \r, \n or both.
        for c in text.replace("\r\n", "\n").replace('\r', "\n").trim_end().chars() {
            let c = if c == '\n' && !add { ' ' } else { c };
            self.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn remember(&mut self, command: &str) {
        self.history.retain(|c| c != command);
        self.history.push(command.to_string());
        let excess = self.history.len().saturating_sub(HISTORY_LEN);
        self.history.drain(..excess);
        if let Some(cache) = &self.lib.cache {
            let _ = fs::create_dir_all(cache);
            let _ = fs::write(cache.join("history"), self.history.join("\n") + "\n");
        }
    }

    /// Tab: the first candidate for the last word, then the next; shift-Tab goes back. A single
    /// candidate is taken, with a space for the next word.
    fn complete(&mut self, back: bool) {
        match &mut self.completion {
            Some(c) => c.step(back),
            None => {
                let Some(mut c) = Completion::new(&self.lib, &self.input) else { return };
                if back {
                    c.step(true);
                }
                self.completion = Some(c);
            }
        }
        let Some(c) = &self.completion else { return };
        self.cursor_back = 0;
        self.input = c.line();
        if c.candidates.len() == 1 {
            self.input.push(' ');
            self.completion = None;
        }
    }

    /// Up (`back`) or Down through the command history; past the newest is an empty line.
    fn recall(&mut self, back: bool) {
        let at = match (self.history_at, back) {
            (None, true) => self.history.len().checked_sub(1),
            (None, false) => None,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) => Some(i + 1).filter(|&i| i < self.history.len()),
        };
        self.history_at = at;
        self.cursor_back = 0;
        self.input = at.map(|i| self.history[i].clone()).unwrap_or_default();
    }

    /// Past commands containing `history_filter`, newest first.
    pub fn history_matches(&self) -> Vec<&String> {
        self.history.iter().rev().filter(|c| c.contains(&self.history_filter)).collect()
    }

    fn open_history(&mut self) {
        self.history_filter = self.input.trim().to_string();
        self.history_sel = 0;
        self.mode = Mode::History;
    }

    fn history_key(&mut self, k: KeyEvent) {
        let n = self.history_matches().len().max(1);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Prompt(Prompt::Command),
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => self.history_sel = (self.history_sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => self.history_sel = (self.history_sel + n - 1) % n,
            KeyCode::Enter => {
                if let Some(c) = self.history_matches().get(self.history_sel) {
                    self.input = c.to_string();
                }
                self.cursor_back = 0;
                self.mode = Mode::Prompt(Prompt::Command);
            }
            _ => {}
        }
    }

    fn text_mut(&mut self, target: TextTarget) -> Option<&mut String> {
        match target {
            TextTarget::Input => Some(&mut self.input),
            TextTarget::Query => Some(&mut self.query),
            TextTarget::SideQuery => Some(&mut self.side_query),
            TextTarget::Filter(i) => self.form.as_mut()?.values.get_mut(i),
            TextTarget::Setup(i) => self.setup.as_mut()?.values.get_mut(i),
        }
    }

    /// Hands a typed text to $EDITOR; it comes back in its place.
    fn edit_in_editor(&mut self, target: TextTarget) {
        let Some(cache) = self.lib.cache.clone() else { return self.status = "no cache folder to edit the text in".into() };
        let text = self.text_mut(target).cloned().unwrap_or_default();
        let path = cache.join("edit.txt");
        if let Err(e) = fs::create_dir_all(&cache).and_then(|_| fs::write(&path, format!("{text}\n"))) {
            return self.status = format!("{}: {e}", path.display());
        }
        self.edit_text = Some((path, target));
    }

    /// The text $EDITOR left in the file, back where it came from: joined into one line, except
    /// in the add prompt, which takes one identifier per line.
    pub fn text_edited(&mut self, path: &std::path::Path, target: TextTarget) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => return self.status = format!("{}: {e}", path.display()),
        };
        let lines = self.mode == Mode::Prompt(Prompt::Add);
        let text = if lines { text.trim_end().to_string() } else { text.split_whitespace().collect::<Vec<_>>().join(" ") };
        if let Some(buf) = self.text_mut(target) {
            *buf = text;
        }
        self.cursor_back = 0;
        match self.mode {
            Mode::Search => self.refilter(),
            Mode::SideSearch => self.refilter_sidebar(true),
            _ => {}
        }
    }

    fn input_key(&mut self, k: KeyEvent) {
        let mode = self.mode;
        if mode == Mode::Prompt(Prompt::Command) && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            return self.recall(k.code == KeyCode::Up);
        }
        if mode == Mode::Prompt(Prompt::Command) && matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            return self.complete(k.code == KeyCode::BackTab);
        }
        self.completion = None;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('g') && mode != Mode::Prompt(Prompt::Secret) {
            let target = match mode {
                Mode::Search => TextTarget::Query,
                Mode::SideSearch => TextTarget::SideQuery,
                _ => TextTarget::Input,
            };
            return self.edit_in_editor(target);
        }
        if mode == Mode::Prompt(Prompt::Command) && ctrl {
            match k.code {
                KeyCode::Char('e') if std::mem::take(&mut self.ctrl_x) => return self.edit_in_editor(TextTarget::Input),
                KeyCode::Char('x') => return self.ctrl_x = true,
                KeyCode::Char('r') => return self.open_history(),
                _ => {}
            }
        }
        self.ctrl_x = false;
        let buf = match mode {
            Mode::Search => &mut self.query,
            Mode::SideSearch => &mut self.side_query,
            _ => &mut self.input,
        };
        match k.code {
            _ if edit_line(buf, &mut self.cursor_back, k.code, ctrl) => {}
            KeyCode::Esc => {
                buf.clear();
                self.cursor_back = 0;
                self.mode = Mode::Normal;
                self.history_at = None;
            }
            KeyCode::Enter => {
                self.cursor_back = 0;
                if let Mode::Prompt(p) = std::mem::replace(&mut self.mode, Mode::Normal) {
                    let text = std::mem::take(&mut self.input);
                    self.history_at = None;
                    if p == Prompt::Command && !text.trim().is_empty() {
                        self.remember(text.trim());
                    }
                    self.submit(p, text.trim());
                }
            }
            _ => {}
        }
        match mode {
            Mode::Search => self.refilter(),
            Mode::SideSearch => self.refilter_sidebar(true),
            // Enter keeps what was typed; Esc clears it.
            Mode::Prompt(Prompt::TableSearch) if self.mode == mode || k.code == KeyCode::Esc => {
                if let Some(t) = &mut self.table {
                    t.query = self.input.clone();
                    t.row = 0;
                }
            }
            Mode::Prompt(Prompt::TableFilter) if self.mode == mode || k.code == KeyCode::Esc => {
                if let Some(t) = &mut self.table {
                    let col = t.col;
                    t.filters.retain(|(c, _)| *c != col);
                    if !self.input.is_empty() {
                        t.filters.push((col, self.input.clone()));
                    }
                    t.row = 0;
                }
            }
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

    fn setup_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(form) = &mut self.setup else { return };
        if form.testing {
            return;
        }
        let n = form.values.len();
        match k.code {
            KeyCode::Esc => {
                self.setup = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => match form.section() {
                Ok((_, setup::Setup::Search(saved))) => {
                    let (name, old) = (form.name().to_string(), form.old.clone());
                    let result = self.lib.save_search(&name, &saved, old.as_deref().filter(|o| *o != name));
                    self.after_config_change(result);
                    self.setup = None;
                    self.mode = Mode::Normal;
                    // Applies the new sort even if only the sort changed.
                    self.shown_filter = Filter::All;
                    self.select_filter(&Filter::Search(name.clone(), saved.query));
                    self.status = format!("saved search '{name}'");
                }
                Ok((section, setup)) => {
                    let secret = form.secret();
                    let (tx, rx) = mpsc::channel();
                    let (probe, test_secret) = (setup.clone(), secret.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(setup::test(&probe, test_secret.as_deref()));
                    });
                    form.testing = true;
                    self.status = "testing the connection…".into();
                    self.setup_job = Some(SetupJob { rx, section, setup, secret });
                }
                Err(e) => self.status = format!("{e:#}"),
            },
            KeyCode::Down | KeyCode::Tab => form.sel = (form.sel + 1) % n,
            KeyCode::Up | KeyCode::BackTab => form.sel = (form.sel + n - 1) % n,
            KeyCode::Right | KeyCode::Left if form.kind == setup::Kind::Llm && form.sel == 1 => form.cycle_provider(k.code == KeyCode::Left),
            // A secret never goes to a file.
            KeyCode::Char('g') if ctrl && !form.masked(form.sel) => {
                let sel = form.sel;
                self.edit_in_editor(TextTarget::Setup(sel));
            }
            code => {
                edit_line(&mut form.values[form.sel], &mut self.cursor_back, code, ctrl);
            }
        }
        if let Some(form) = &mut self.setup
            && form.kind == setup::Kind::Llm
            && form.sel == 1
        {
            form.provider_changed();
        }
    }

    /// Saves a `:setup` form whose connection test passed: the secret to the secret store, the rest to yeda.toml.
    pub fn poll_setup(&mut self) {
        let Some(job) = &self.setup_job else { return };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("the connection test stopped")),
        };
        let job = self.setup_job.take().unwrap();
        let Some(form) = &mut self.setup else { return };
        form.testing = false;
        if let Err(e) = result {
            self.status = format!("not saved: {e:#}");
            return;
        }
        let (name, kind) = (form.name().to_string(), form.kind);
        let mut stored = String::new();
        if let Some(secret) = &job.secret {
            let account = setup::account(&job.setup);
            if let Err(e) = yeda_secrets::set(&account, secret) {
                stored = format!(" (secret kept for this session only: {e:#})");
            }
            self.secrets.insert(account.clone(), secret.clone());
            self.rejected.remove(&account);
        }
        if kind == setup::Kind::Openalex {
            self.setup = None;
            self.mode = Mode::Normal;
            self.status = format!("saved the OpenAlex key to the secret store{stored}");
            return;
        }
        let section = if kind == setup::Kind::Llm { "llm" } else { "locations" };
        let result = self.lib.update_config(|t| {
            let entry = config_section(t, section)?.entry(name.clone()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if !entry.is_table() {
                *entry = toml::Value::Table(toml::Table::new());
            }
            let table = entry.as_table_mut().unwrap();
            if kind == setup::Kind::Llm {
                table.remove("base_url");
            }
            table.extend(job.section);
            Ok(())
        });
        self.after_config_change(result);
        self.setup = None;
        self.mode = Mode::Normal;
        self.status = format!("saved [{section}.{name}] to yeda.toml{stored}");
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
            KeyCode::Down => form.select((form.sel + 1) % n, &self.lib.refs),
            KeyCode::Char('d') if ctrl => form.select((form.sel + 1) % n, &self.lib.refs),
            KeyCode::Up => form.select((form.sel + n - 1) % n, &self.lib.refs),
            KeyCode::Char('u') if ctrl => form.select((form.sel + n - 1) % n, &self.lib.refs),
            KeyCode::Char('n') if ctrl => form.pick = (form.pick + 1).min(form.suggestions().len().saturating_sub(1)),
            KeyCode::Char('p') if ctrl => form.pick = form.pick.saturating_sub(1),
            KeyCode::Tab => form.accept(),
            KeyCode::Right if self.cursor_back == 0 => form.accept(),
            KeyCode::Char('l') if ctrl => {
                form.values[form.sel].clear();
                form.pick = 0;
            }
            KeyCode::Char('x') if ctrl => {
                form.values.iter_mut().for_each(String::clear);
                form.other.clear();
                form.pick = 0;
            }
            KeyCode::Char('g') if ctrl => {
                let sel = form.sel;
                self.edit_in_editor(TextTarget::Filter(sel));
            }
            code => {
                if edit_line(&mut form.values[form.sel], &mut self.cursor_back, code, ctrl) {
                    form.pick = 0;
                }
            }
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

    /// Runs `command <table> rest`: with the only table, else the one picked in a modal.
    fn pick_table(&mut self, command: &str, rest: &str) {
        match self.lib.tables.len() {
            0 => self.status = "no tables defined: add [tables.<name>] to yeda.toml or run import-table".into(),
            1 => {
                let name = self.lib.tables.keys().next().unwrap().clone();
                self.submit(Prompt::Command, format!("{command} {name} {rest}").trim());
            }
            _ => {
                self.table_pick = Some((command.to_string(), rest.to_string()));
                self.table_sel = self.table.as_ref().and_then(|t| self.lib.tables.keys().position(|n| *n == t.name)).unwrap_or(0);
                self.mode = Mode::TablePick;
            }
        }
    }

    fn table_pick_key(&mut self, k: KeyEvent) {
        let n = self.lib.tables.len().max(1);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.table_pick = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => self.table_sel = (self.table_sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => self.table_sel = (self.table_sel + n - 1) % n,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                let name = self.lib.tables.keys().nth(self.table_sel).cloned();
                if let (Some((command, rest)), Some(name)) = (self.table_pick.take(), name) {
                    self.submit(Prompt::Command, format!("{command} {name} {rest}").trim());
                }
            }
            _ => {}
        }
    }

    /// The PDF of reference `i` a command uses: its only one, the one picked for this run, else a
    /// picker after which `retry` runs again. `None` while picking; `Some(None)` when it has none.
    fn pdf_for(&mut self, i: usize, retry: Retry) -> Option<Option<Pdf>> {
        let r = &self.lib.refs[i];
        if let Some(pdf) = self.pdf_chosen.get(&r.key).and_then(|id| r.pdfs.iter().find(|p| p.id == *id)) {
            return Some(Some(pdf.clone()));
        }
        if r.pdfs.len() < 2 {
            return Some(r.pdfs.first().cloned());
        }
        let mut pdfs: Vec<(Pdf, usize)> = r.pdfs.iter().map(|p| (p.clone(), self.load_annotations(&r.key, p).len())).collect();
        pdfs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        self.pdf_pick = Some(PdfPick { key: r.key.clone(), pdfs, retry });
        self.pdf_sel = 0;
        self.mode = Mode::PdfPick;
        None
    }

    fn pdf_pick_key(&mut self, k: KeyEvent) {
        let n = self.pdf_pick.as_ref().map_or(1, |p| p.pdfs.len().max(1));
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.pdf_pick = None;
                self.pdf_chosen.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => self.pdf_sel = (self.pdf_sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => self.pdf_sel = (self.pdf_sel + n - 1) % n,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if let Some(pick) = self.pdf_pick.take()
                    && let Some((pdf, _)) = pick.pdfs.get(self.pdf_sel)
                {
                    self.pdf_chosen.insert(pick.key, pdf.id.clone());
                    self.retry(pick.retry);
                }
            }
            _ => {}
        }
    }

    fn open_help(&mut self, page: HelpPage) {
        self.help_scroll = 0;
        self.help_query.clear();
        self.help_typing = false;
        self.mode = Mode::Help(page);
    }

    fn help_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page / 2).max(1) as u16;
        if self.help_typing {
            match k.code {
                KeyCode::Esc => {
                    self.help_query.clear();
                    self.help_typing = false;
                }
                KeyCode::Enter => {
                    self.help_typing = false;
                    self.help_jump(true, true);
                }
                KeyCode::Backspace => {
                    self.help_query.pop();
                }
                KeyCode::Char(c) if !ctrl => self.help_query.push(c),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Esc if !self.help_query.is_empty() => self.help_query.clear(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Char('/') => {
                self.help_query.clear();
                self.help_typing = true;
            }
            KeyCode::Char('n') => self.help_jump(true, false),
            KeyCode::Char('N') => self.help_jump(false, false),
            KeyCode::Char('d') if ctrl => self.help_scroll = self.help_scroll.saturating_add(half),
            KeyCode::Char('u') if ctrl => self.help_scroll = self.help_scroll.saturating_sub(half),
            KeyCode::Char('j') | KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::Char('G') => self.help_scroll = u16::MAX,
            _ => {}
        }
    }

    /// Scrolls the next (or previous) line matching `help_query` to the top; `here` includes the top line.
    fn help_jump(&mut self, forward: bool, here: bool) {
        let query = self.help_query.to_lowercase();
        if query.is_empty() {
            return;
        }
        let hits: Vec<usize> = self.help_lines.iter().enumerate().filter(|(_, l)| l.to_lowercase().contains(&query)).map(|(i, _)| i).collect();
        let top = self.help_scroll as usize;
        let next = if forward {
            hits.iter().find(|&&i| i > top || (here && i == top)).or(hits.first())
        } else {
            hits.iter().rfind(|&&i| i < top).or(hits.last())
        };
        match next {
            Some(&i) => {
                self.help_scroll = i as u16;
                let at = hits.iter().position(|&h| h == i).unwrap_or(0) + 1;
                self.status = format!("/{}: {at} of {}", self.help_query, hits.len());
            }
            None => self.status = format!("/{}: not found", self.help_query),
        }
    }

    fn open_chooser(&mut self, kind: PickKind) {
        let targets = self.targets();
        let tags: Vec<String> = match targets.first() {
            Some(&first) => {
                let refs = &self.lib.refs;
                refs[first].tags.iter().filter(|t| targets.iter().all(|&i| refs[i].tags.contains(t))).cloned().collect()
            }
            None if kind == PickKind::AddTo => vec![],
            None => return self.status = "no reference selected".into(),
        };
        self.chooser = Some(Chooser { kind, initial: tags.clone(), tags, options: vec![], sel: 0, moved: false });
        self.input.clear();
        self.mode = Mode::Pick;
        self.refresh_options();
    }

    /// Rebuilds the dropdown from the input: fuzzy matches ranked by score, then create/remove entries.
    fn refresh_options(&mut self) {
        let current_coll = match self.chooser.as_ref().map(|c| c.kind) {
            // New references go where the library is being looked at, unless changed.
            Some(PickKind::AddTo) => match self.current_filter() {
                Filter::Collection(p) => Some(p.clone()),
                _ => None,
            },
            _ => self.selected().and_then(|r| r.collections.first().cloned()),
        };
        let Some(c) = &mut self.chooser else { return };
        let query = self.input.trim().to_string();
        let mut candidates: Vec<String> = match c.kind {
            PickKind::Tags => c.tags.clone(),
            PickKind::Move | PickKind::AddTo => vec![],
        };
        for f in &self.sidebar {
            match (c.kind, f) {
                (PickKind::Tags, Filter::Tag(t)) if !candidates.contains(t) => candidates.push(t.clone()),
                (PickKind::Move | PickKind::AddTo, Filter::Collection(p)) => candidates.push(p.clone()),
                _ => {}
            }
        }
        let mut scored: Vec<(u32, &String)> = candidates
            .iter()
            .filter_map(|t| if query.is_empty() { Some((0, t)) } else { self.searcher.score(&query, t).map(|s| (s, t)) })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        c.options = scored
            .into_iter()
            .map(|(_, t)| {
                let mark = match c.kind {
                    PickKind::Tags if c.tags.contains(t) => "✓ ",
                    PickKind::Move | PickKind::AddTo if current_coll.as_ref() == Some(t) => "• ",
                    _ => "  ",
                };
                (format!("{mark}{t}"), Pick::Existing(t.clone()))
            })
            .collect();
        if !query.is_empty() && !candidates.contains(&query) {
            let what = if c.kind == PickKind::Tags { "tag" } else { "collection" };
            c.options.push((format!("+ new {what} \"{query}\""), Pick::Create(query.clone())));
        }
        if c.kind == PickKind::Move && current_coll.is_some() {
            c.options.push(("— remove from its collection".into(), Pick::Unfile));
        }
        if c.kind == PickKind::AddTo {
            c.options.push(("— no collection".into(), Pick::Unfile));
        }
        c.sel = match (c.kind, &current_coll) {
            (PickKind::Move | PickKind::AddTo, Some(p)) if query.is_empty() => c.options.iter().position(|(_, o)| *o == Pick::Existing(p.clone())).unwrap_or(0),
            _ => 0,
        };
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
            KeyCode::Backspace if self.input.is_empty() && c.kind == PickKind::Tags => {
                c.tags.pop();
                self.refresh_options();
            }
            KeyCode::Char('g') if ctrl => self.edit_in_editor(TextTarget::Input),
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
                    (PickKind::AddTo, choice) => {
                        let collection = match choice {
                            Pick::Existing(p) | Pick::Create(p) => Some(p),
                            Pick::Unfile => None,
                        };
                        let (text, back) = self.pending_add.take().unwrap_or((String::new(), Mode::Normal));
                        self.close_chooser();
                        self.mode = back;
                        let todo: std::collections::VecDeque<String> = ids::split_ids(&text).into_iter().map(Into::into).collect();
                        self.status = format!("fetching 1/{}: {}…", todo.len(), todo[0]);
                        self.job = Some(Job::Fetch(Batch { step: self.step.clone(), collection, total: todo.len(), todo, added: vec![], existed: vec![], failed: vec![] }));
                    }
                    (PickKind::Tags, Pick::Unfile) => {}
                }
            }
            code => {
                if edit_line(&mut self.input, &mut self.cursor_back, code, ctrl) {
                    self.refresh_options();
                }
            }
        }
    }

    /// Saves edited tags (a move is applied on Enter, so closing a move picker changes nothing).
    fn close_chooser(&mut self) {
        self.mode = Mode::Normal;
        self.input.clear();
        if let Some((_, back)) = self.pending_add.take() {
            self.mode = back;
            self.status = "nothing added".into();
        }
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
        if p == Prompt::Command && self.step.label.is_empty() {
            self.step.label = text.to_string();
        }
        match p {
            Prompt::Command => match text.split_once(' ').unwrap_or((text, "")) {
                ("q" | "q!" | "quit", _) => self.quit = true,
                ("save", name) if !name.trim().is_empty() => self.save_search(name.trim()),
                ("save", _) => self.status = "usage: :save <name>".into(),
                (cmd @ ("summarize" | "summarize!"), args) if args.trim().is_empty() && self.lib.tables.len() > 1 => self.pick_table(cmd, ""),
                (cmd @ ("summarize" | "summarize!"), args) => {
                    if let Err(e) = self.summarize(args, cmd.ends_with('!'), text) {
                        self.status = format!("{e:#}");
                    }
                }
                ("help", _) => self.open_help(HelpPage::Guide),
                ("keys", _) => self.open_help(HelpPage::Keys),
                ("secrets", _) => self.open_secrets(),
                ("discover", rest) => {
                    if let Err(e) = self.discover(rest, text) {
                        self.status = format!("{e:#}");
                    }
                }
                ("found", _) if self.found.is_empty() => self.status = "nothing found yet: :discover".into(),
                ("found", _) => self.mode = Mode::Found,
                ("history", _) => {
                    self.input.clear();
                    self.open_history();
                }
                ("setup", rest) => {
                    let (kind, name) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
                    let kind = match kind {
                        "llm" => setup::Kind::Llm,
                        "webdav" => setup::Kind::Webdav,
                        "openalex" => setup::Kind::Openalex,
                        _ => {
                            self.status = "usage: :setup llm|webdav [name], or :setup openalex".into();
                            return;
                        }
                    };
                    self.setup = Some(SetupForm::new(kind, name.trim(), &self.lib));
                    self.mode = Mode::Setup;
                }
                ("failed", _) if self.report.is_empty() => self.status = "nothing failed".into(),
                ("failed", _) => self.open_help(HelpPage::Failed),
                ("log", _) => match self.log.path.clone() {
                    Some(path) => {
                        let lines = fs::read_to_string(&path).map_or(1, |t| t.lines().count().max(1));
                        self.edit = Some((path, Some(lines)));
                    }
                    None => self.status = "no folder for the log".into(),
                },
                ("cancel", _) => match &self.summarizer {
                    Some(s) => {
                        s.cancel.store(true, Ordering::Relaxed);
                        self.status = "cancelling after the current paper…".into();
                    }
                    None => self.status = "nothing to cancel".into(),
                },
                ("export", args) => self.export(args),
                ("attach", args) => self.attach(args, text),
                ("detach", _) => self.detach(text),
                ("sync", _) => self.start_git("sync", crate::sync::run),
                ("git-init", url) => {
                    let url = Some(url.trim().to_string()).filter(|u| !u.is_empty());
                    self.start_git("git-init", move |dir, log| crate::sync::init(dir, url.as_deref(), log));
                }
                (cmd @ ("graph" | "graph!"), args) => self.graph_command(args, cmd.ends_with('!'), text),
                ("table", args) => self.table_command(args.trim()),
                ("fetch", args) => self.fetch_command(args.trim(), text),
                ("cite", args) => self.cite(args.trim()),
                ("", _) => {}
                (name, args) => {
                    if !self.action(name, args.trim()) {
                        self.status = format!("unknown command: {text}");
                    }
                }
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
                if text.is_empty() {
                    self.asking = None;
                    self.status = "cancelled".into();
                    return;
                }
                let Some(p) = self.asking.take() else { return };
                self.secrets.insert(p.account.clone(), text.into());
                self.rejected.remove(&p.account);
                let saved = yeda_secrets::set(&p.account, text);
                self.status = "saved".into();
                self.retry(p.retry);
                // After the retry, so its own status doesn't hide that the secret wasn't saved.
                if let Err(e) = saved {
                    self.status = format!("kept for this session only, not saved: {e:#}; {}", self.status);
                }
            }
            Prompt::Add if !ids::split_ids(text).is_empty() => {
                // The add prompt itself has already closed; a :discover list stays open.
                let back = if self.mode == Mode::Found { Mode::Found } else { Mode::Normal };
                self.pending_add = Some((text.to_string(), back));
                self.open_chooser(PickKind::AddTo);
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
        let saved = library::SavedSearch { query: query.clone(), sort: Some(self.sort.field), descending: self.sort.descending };
        let result = self.lib.save_search(name, &saved, None);
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
        self.status = format!("status: {}{}", probe.status, plural_suffix(targets.len()));
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
    /// The API key stored for the profile's server, else a prompt naming the server, after which
    /// `command` runs again. `None` while prompting.
    fn llm_key(&mut self, profile: &yeda_core::llm::Profile, command: &str) -> Option<Option<String>> {
        let account = yeda_secrets::llm_account(profile.url());
        if profile.needs_key() {
            self.secret(&account, &format!("API key sent to {}", profile.url()), Retry::Command(command.into())).map(Some)
        } else {
            Some(yeda_secrets::get(&account).ok().flatten())
        }
    }

    /// `:discover [what]`: asks the default LLM profile for papers like the selected or marked ones, or as described.
    fn discover(&mut self, prompt: &str, command: &str) -> anyhow::Result<()> {
        if self.discover_job.is_some() {
            anyhow::bail!("already searching");
        }
        // With a description, only marked references go along, not whichever one happens to be selected.
        let targets = if prompt.trim().is_empty() || !self.marked.is_empty() { self.targets() } else { Vec::new() };
        if prompt.trim().is_empty() && targets.is_empty() {
            anyhow::bail!("usage: :discover <what to look for>, or select references first");
        }
        let (name, profile) = self.lib.llm_profile(None)?;
        let Some(key) = self.llm_key(&profile, command) else { return Ok(()) };
        let from: Vec<&Ref> = targets.iter().map(|&i| &self.lib.refs[i]).collect();
        let openalex = self.secrets.get(OPENALEX_ACCOUNT).cloned();
        self.discover_job = Some(crate::discover::spawn(profile, key, prompt, &from, openalex));
        self.status = format!("asking {name} for papers{}…", if from.is_empty() { String::new() } else { format!(" like {} references", from.len()) });
        Ok(())
    }

    /// Runs `task` in the background, showing git's output in a modal as it comes.
    fn start_git(&mut self, title: &str, task: impl FnOnce(&std::path::Path, crate::sync::Log) -> anyhow::Result<String> + Send + 'static) {
        if self.git.as_ref().is_some_and(|g| g.done.is_none()) {
            self.mode = Mode::Git;
            return;
        }
        let (tx, rx) = mpsc::channel();
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            let lines = tx.clone();
            let result = task(&dir, &move |line| drop(lines.send(GitMsg::Line(line))));
            let _ = tx.send(GitMsg::Done(result));
        });
        self.git = Some(GitView { title: title.into(), lines: vec![], done: None, rx });
        self.mode = Mode::Git;
    }

    pub fn poll_git(&mut self) {
        let Some(g) = &mut self.git else { return };
        let mut finished = false;
        while let Ok(msg) = g.rx.try_recv() {
            match msg {
                GitMsg::Line(line) => g.lines.push(line),
                GitMsg::Done(result) => {
                    g.done = Some(result.unwrap_or_else(|e| format!("{}: {e:#}", g.title)));
                    finished = true;
                }
            }
        }
        if finished {
            self.status = g.done.clone().unwrap_or_default();
            self.reload();
        }
    }

    fn git_key(&mut self, k: KeyEvent) {
        if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            self.mode = Mode::Normal;
        }
    }

    pub fn poll_discover(&mut self) {
        let Some(rx) = &self.discover_job else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("the search stopped")),
        };
        self.discover_job = None;
        match result {
            Ok(found) if found.is_empty() => self.status = "discover: no papers found".into(),
            Ok(found) => {
                self.status = format!("discover: {} papers, {} found in OpenAlex", found.len(), found.iter().filter(|c| c.work.is_some()).count());
                self.found = found;
                // The LLM only knows the papers it was given, so it suggests others the library has.
                let mut found = std::mem::take(&mut self.found);
                found.sort_by_key(|c| self.found_in_library(c));
                self.found = found;
                self.found_sel = 0;
                self.mode = Mode::Found;
            }
            Err(e) => self.status = format!("discover: {e:#}"),
        }
    }

    /// Whether the suggestion is in the library already, by DOI or title.
    pub fn found_in_library(&self, c: &Candidate) -> bool {
        let doi = c.work.as_ref().and_then(|w| w.doi.clone()).or(c.found.doi.clone());
        doi.is_some_and(|d| self.lib.find_doi(&d).is_some()) || self.lib.refs.iter().any(|r| graph::same_title(&r.title, c.title()))
    }

    fn found_key(&mut self, k: KeyEvent) {
        let n = self.found.len();
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.found_sel = (self.found_sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => self.found_sel = self.found_sel.saturating_sub(1),
            KeyCode::Char('a') | KeyCode::Enter => match self.found.get(self.found_sel) {
                Some(c) if self.found_in_library(c) => self.status = "already in the library".into(),
                Some(c) => match c.id() {
                    Some(id) if self.job.is_none() => self.submit(Prompt::Add, &id),
                    Some(_) => self.status = "still adding; try again in a moment".into(),
                    None => self.status = "no DOI or URL to add it by".into(),
                },
                None => {}
            },
            KeyCode::Char('o') => {
                if let Some(id) = self.found.get(self.found_sel).and_then(Candidate::id) {
                    let url = if id.starts_with("10.") { format!("https://doi.org/{id}") } else { id };
                    self.xdg_open(url);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Asks y/n before `command` runs on `n` references, unless it is one or this run was confirmed;
    /// true when it may go ahead now.
    fn confirm_many(&mut self, what: &str, n: usize, command: &str) -> bool {
        if std::mem::take(&mut self.confirmed) || n < 2 {
            return true;
        }
        self.status = format!("{what} {n} references? (y/n)");
        self.confirming = Some(("command".into(), command.into()));
        self.mode = Mode::Confirm;
        false
    }

    fn summarize(&mut self, args: &str, overwrite: bool, command: &str) -> anyhow::Result<()> {
        if self.summarizer.is_some() {
            anyhow::bail!("a summarization is already running (:cancel stops it)");
        }
        let mut words = args.split_whitespace();
        let table_name = self.table_name(words.next())?;
        let (profile_name, profile) = self.lib.llm_profile(words.next())?;
        let table = self.lib.tables[&table_name].clone();
        let targets = self.targets();
        if targets.is_empty() {
            anyhow::bail!("no reference selected");
        }
        let mut jobs: Vec<tables::Job> = Vec::new();
        for &i in &targets {
            let Some(pdf) = self.pdf_for(i, Retry::Command(command.into())) else { return Ok(()) };
            let r = &self.lib.refs[i];
            let annots = pdf.as_ref().map(|p| self.load_annotations(&r.key, p)).unwrap_or_default();
            jobs.extend(tables::plan(&self.lib, &table_name, &table, r, pdf.as_ref(), &annots, overwrite));
        }
        if jobs.is_empty() {
            anyhow::bail!("nothing to fill in '{table_name}' (add ! to regenerate existing cells)");
        }
        let Some(key) = self.llm_key(&profile, command) else { return Ok(()) };
        if !self.confirm_many(&format!("summarize into {table_name}"), jobs.len(), command) {
            return Ok(());
        }
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
            step: self.step.clone(),
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
                Ok(WorkerMsg::Started(text)) => {
                    self.log.write(&text);
                    self.status = text;
                }
                Ok(WorkerMsg::Finished { key, result: Ok((row, note)) }) => {
                    if let Some(i) = self.lib.refs.iter().position(|r| r.key == key) {
                        tables::merge(&mut self.lib.refs[i], &s.table, row, s.overwrite);
                        let saved = self.lib.save_ai(&self.lib.refs[i]).and_then(|e| self.undo.borrow_mut().apply(&self.dir, &s.step, [e]));
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
            Job::Download(pdf, password) => self.download(&pdf, &password),
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
            Retry::Secrets => self.open_secrets(),
        }
    }

    /// Every secret the configuration can use, with where each is set.
    fn open_secrets(&mut self) {
        let mut items: Vec<(String, String)> = Vec::new();
        for (name, dav) in &self.lib.remotes {
            items.push((yeda_secrets::webdav_account(&dav.user, &dav.url), format!("WebDAV {name}: {}@{}", dav.user, dav.url)));
        }
        for (name, profile) in self.lib.llm.iter().filter(|(_, p)| p.needs_key()) {
            items.push((yeda_secrets::llm_account(profile.url()), format!("LLM {name}: {}", profile.url())));
        }
        items.push((OPENALEX_ACCOUNT.into(), "OpenAlex API key, optional (free at openalex.org/settings/api)".into()));
        let mut seen = HashSet::new();
        items.retain(|(account, _)| seen.insert(account.clone()));
        self.secret_items = items
            .into_iter()
            .map(|(account, label)| {
                let status = if self.secrets.contains_key(&account) {
                    "this session".into()
                } else {
                    match yeda_secrets::get(&account) {
                        Ok(Some(_)) => "in the secret store".into(),
                        Ok(None) => "not set".into(),
                        Err(e) => {
                            self.status = format!("{e:#}");
                            "secret store unavailable".into()
                        }
                    }
                };
                SecretItem { account, label, status }
            })
            .collect();
        self.secret_sel = self.secret_sel.min(self.secret_items.len() - 1);
        self.mode = Mode::Secrets;
    }

    fn secrets_key(&mut self, k: KeyEvent) {
        let Some(item) = self.secret_items.get(self.secret_sel) else { return };
        let account = item.account.clone();
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.secret_sel = (self.secret_sel + 1).min(self.secret_items.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => self.secret_sel = self.secret_sel.saturating_sub(1),
            KeyCode::Enter => {
                self.asking = Some(PendingSecret { account, label: item.label.clone(), retry: Retry::Secrets });
                self.input.clear();
                self.mode = Mode::Prompt(Prompt::Secret);
            }
            KeyCode::Char('d') => {
                self.secrets.remove(&account);
                self.status = match yeda_secrets::delete(&account) {
                    Ok(()) => "removed".into(),
                    Err(e) => format!("{e:#}"),
                };
                let status = std::mem::take(&mut self.status);
                self.open_secrets();
                self.status = status;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Brings the cached copy of the selected reference's WebDAV PDF up to date, then opens it.
    fn download(&mut self, pdf: &Pdf, password: &str) {
        let (Some(remote), Ok(path)) = (self.lib.remote_pdf(pdf), self.lib.pdf_path(pdf)) else { return };
        let account = yeda_secrets::webdav_account(&remote.dav.user, &remote.dav.url);
        match yeda_fs::sync_cached(&path, |cached| {
            yeda_net::webdav::current_version(remote.dav, password, &remote.path).map(|v| v.is_some() && v.as_deref() == cached)
        }, || yeda_net::webdav::download(remote.dav, password, &remote.path)) {
            Ok(()) => self.launch_reader(&path, pdf),
            Err(e) if yeda_net::webdav::is_unauthorized(&e) => {
                self.reject(&account);
                self.open_pdf();
            }
            Err(e) if path.exists() => {
                self.status = format!("{e:#}; opened the cached copy");
                self.launch_reader(&path, pdf);
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
        let file = unquote(file);
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

    fn detach(&mut self, command: &str) {
        let Some(&i) = self.visible.get(self.sel) else { return };
        let Some(pdf) = self.pdf_for(i, Retry::Command(command.into())) else { return };
        let Some(pdf) = pdf else { return self.status = "no pdf".into() };
        let mut r = self.lib.refs[i].clone();
        r.pdfs.retain(|p| p.id != pdf.id);
        let effects = self.lib.save(&r).map(|e| vec![e, Effect::remove(library::pdf_annotations_dir(&r.key, &pdf))]);
        if self.apply(effects) {
            self.status = format!("detached {}:{}", pdf.location, pdf.path);
            self.reload();
        }
    }

    /// Copies `file` to a local location, or uploads it to a WebDAV one keeping a cached copy;
    /// returns reference `i` pointing at it, not yet saved.
    fn attach_file(&self, i: usize, file: &std::path::Path, location: &str, password: Option<&str>) -> anyhow::Result<Ref> {
        let bytes = fs::read(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
        let mut r = self.lib.refs[i].clone();
        let pdf = self.lib.attach(&r, location, &file.to_string_lossy())?;
        let path = self.lib.pdf_path(&pdf)?;
        yeda_fs::write_atomic(&path, &bytes)?;
        if let (Some(remote), Some(password)) = (self.lib.remote_pdf(&pdf), password) {
            yeda_net::webdav::upload(remote.dav, password, &remote.path, &bytes)?;
            let version = yeda_net::webdav::current_version(remote.dav, password, &remote.path)?;
            yeda_fs::set_cached_version(&path, version.as_deref())?;
        }
        r.pdfs.push(pdf);
        Ok(r)
    }

    fn finish_attach(&mut self, file: &std::path::Path, location: &str, password: Option<&str>) {
        let Some(&i) = self.visible.get(self.sel) else { return };
        match self.attach_file(i, file, location, password) {
            Ok(r) => {
                let path = r.pdfs.last().map(|p| format!("{}:{}", p.location, p.path)).unwrap_or_default();
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

    /// Extracts the annotations of every PDF of reference `i`.
    fn extract_from(&mut self, i: usize) -> anyhow::Result<usize> {
        let r = &self.lib.refs[i];
        anyhow::ensure!(!r.pdfs.is_empty(), "no pdf");
        let mut total = 0;
        for p in &r.pdfs {
            let pdf = self.lib.pdf_path(p)?;
            let bytes = fs::read(&pdf).map_err(|e| anyhow::anyhow!("{}: {e}", pdf.display()))?;
            let words = poppler::word_boxes(&pdf)?;
            let dir = library::pdf_annotations_dir(&r.key, p);
            total += yeda_fs::locked(&self.path(&dir), || {
                let existing = yeda_fs::read_optional(&self.path(&annotations::file_path(&r.key, p)))?;
                let mut render = |page, height, rect| poppler::render_region(&pdf, page, height, rect).ok();
                let (n, effects) = annotations::sync_from_pdf(&dir, existing.as_deref(), &bytes, Some(&words), &mut render)?;
                self.write(&self.step, effects)?;
                Ok(n)
            })?;
        }
        Ok(total)
    }

    fn load_annotations(&self, key: &str, pdf: &Pdf) -> Vec<Annotation> {
        let text = yeda_fs::read_optional(&self.path(&annotations::file_path(key, pdf))).ok().flatten();
        text.and_then(|t| annotations::parse(&t).ok()).unwrap_or_default()
    }

    /// Loads the selected ref's annotations and the selected annotation's image, if they changed.
    pub fn sync_annots(&mut self) {
        let key = self.selected().map(|r| r.key.clone());
        let pdfs = self.selected().map(|r| r.pdfs.clone()).unwrap_or_default();
        let files: Vec<PathBuf> = key.iter().flat_map(|k| pdfs.iter().map(|p| self.path(&annotations::file_path(k, p)))).collect();
        let modified: Vec<_> = files.iter().map(|f| fs::metadata(f).ok()?.modified().ok()).collect();
        if key != self.annots_key || modified != self.annots_modified {
            self.annots_modified = modified;
            let same_paper = key == self.annots_key;
            self.annots = Vec::new();
            for (pdf, file) in pdfs.iter().zip(&files) {
                match yeda_fs::read_optional(file).and_then(|t| t.as_deref().map(annotations::parse).transpose()) {
                    Ok(annots) => self.annots.extend(annots.unwrap_or_default().into_iter().map(|a| (pdf.clone(), a))),
                    Err(e) => self.status = format!("{e:#}"),
                }
            }
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
            (Some(k), Some((pdf, Annotation { image: Some(img), .. }))) => Some(self.path(&library::pdf_annotations_dir(k, pdf)).join(img)),
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
    fn add_one(&mut self, input: &str, collection: Option<&String>, step: &undo::Step) -> anyhow::Result<(String, bool)> {
        let mut r = wand::fetch(input)?;
        if let Some(x) = r.doi.as_deref().and_then(|d| self.lib.find_doi(d)) {
            return Ok((x.key.clone(), true));
        }
        r.key = self.lib.unique_key(&r);
        r.collections = collection.into_iter().cloned().collect();
        self.write(step, vec![self.lib.save(&r)?])?;
        self.reload();
        Ok((r.key, false))
    }

    /// Adds the batch's next identifier, then queues the rest so the status line shows progress.
    fn add_next(&mut self, mut batch: Batch) {
        let Some(input) = batch.todo.pop_front() else { return self.finish_batch(batch) };
        match self.add_one(&input, batch.collection.as_ref(), &batch.step) {
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
        for line in &self.report {
            self.log.write(&format!("failed: {line}"));
        }
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
        if self.right == view {
            return self.back_to_details();
        }
        self.right = view;
        self.pane = Pane::Right;
    }

    /// Closes the right column's view; a maximized right column goes back to all columns.
    fn back_to_details(&mut self) {
        if self.pane == Pane::Right {
            self.focus = false;
        }
        self.right = RightView::Details;
        self.pane = Pane::List;
    }

    fn table_command(&mut self, args: &str) {
        if args == "close" {
            return self.close_table();
        }
        if let Some(rest) = args.strip_prefix("rename") {
            let [old, new] = rest.split_whitespace().collect::<Vec<_>>()[..] else {
                return self.status = "usage: :table rename <old> <new>".into();
            };
            let effects = self.lib.rename_table(old, new);
            if self.apply(effects) {
                self.reload();
                if let Some(t) = self.table.as_mut().filter(|t| t.name == old) {
                    t.name = new.into();
                }
                self.status = format!("renamed table {old} to {new}");
            }
            return;
        }
        if args.is_empty() {
            return self.pick_table("table", "");
        }
        let name = match self.table_name(Some(args)) {
            Ok(n) => n,
            Err(e) => return self.status = format!("{e:#}"),
        };
        let keys: Vec<String> = self.targets().iter().map(|&i| self.lib.refs[i].key.clone()).collect();
        if keys.is_empty() {
            return self.status = "no reference selected".into();
        }
        self.table = Some(TableView::new(name, keys));
        self.right = RightView::Tables;
        self.pane = Pane::Right;
    }

    fn close_table(&mut self) {
        self.table = None;
        if self.right == RightView::Tables {
            self.back_to_details();
        }
    }

    /// The table's rows as shown; empty without a table.
    pub fn table_rows(&mut self) -> Vec<Vec<String>> {
        self.table.as_ref().map(|t| t.rows(&self.lib, &mut self.searcher)).unwrap_or_default()
    }

    /// Opens the selected cell's file in $EDITOR at the cell: the LLM file, or the reference for its own fields.
    fn table_edit(&mut self) {
        let rows = self.table_rows();
        let Some(t) = &self.table else { return };
        let Some(key) = rows.get(t.row).map(|r| r[0].clone()) else { return };
        let Some(table::Column { table: Some(name), name: column }) = table::columns(&self.lib, &t.name).into_iter().nth(t.col) else {
            self.edit = Some((self.path(&library::ref_path(&key)), None));
            return;
        };
        let path = self.path(&library::ai_path(&key));
        let find = |path: &PathBuf| fs::read_to_string(path).ok().and_then(|text| table::line_of(&text, &name, &column));
        if find(&path).is_none()
            && let Some(i) = self.lib.refs.iter().position(|r| r.key == key)
        {
            // A cell not in the file yet gets an empty line to fill in.
            tables::merge(&mut self.lib.refs[i], &name, Row::from([(column.clone(), String::new())]), false);
            let effect = self.lib.save_ai(&self.lib.refs[i]);
            self.apply(effect.map(|e| vec![e]));
        }
        self.edit = Some((path.clone(), find(&path)));
    }

    /// Clicks select and focus; the wheel moves the selection of the column under the pointer.
    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.mode != Mode::Normal {
            return;
        }
        let at = Position::new(m.column, m.row);
        // Lists are recorded after their columns, so the last hit is the most specific.
        let Some(&(pane, area, offset)) = self.clickable.iter().rev().find(|(_, r, _)| r.contains(at)) else { return };
        if pane == Pane::Right && self.right == RightView::Details {
            return;
        }
        // The graph's own area, as opposed to the citation list under it.
        let on_graph = pane == Pane::Right && offset.is_none() && self.right == RightView::Graph;
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if on_graph => {
                if let Some(g) = &mut self.graph {
                    g.scroll = if m.kind == MouseEventKind::ScrollDown { g.scroll + 3 } else { g.scroll.saturating_sub(3) };
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.pane = pane;
                if let Some(offset) = offset {
                    self.move_to(offset + (m.row - area.y) as usize);
                }
            }
            MouseEventKind::ScrollDown => {
                self.pane = pane;
                self.step(3);
            }
            MouseEventKind::ScrollUp => {
                self.pane = pane;
                self.step(-3);
            }
            _ => {}
        }
    }

    fn step(&mut self, delta: isize) {
        let cur = match self.pane {
            Pane::Sidebar => self.side_sel,
            Pane::List => self.sel,
            Pane::Right if self.right == RightView::Tables => self.table.as_ref().map_or(0, |t| t.row),
            Pane::Right if self.right == RightView::Graph => self.graph.as_ref().map_or(0, |g| g.nbr),
            Pane::Right if self.right == RightView::Clusters => self.cluster.as_ref().map_or(0, |c| c.nbr),
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
            Pane::Right if self.right == RightView::Clusters => {
                if let Some(c) = &mut self.cluster {
                    c.nbr = i.min(c.entries().len().saturating_sub(1));
                }
            }
            Pane::Right if self.right == RightView::Graph => {
                if let Some(g) = &mut self.graph {
                    g.nbr = i.min(g.entries().len().saturating_sub(1));
                }
            }
            Pane::Right if self.right == RightView::Tables => {
                let n = self.table_rows().len();
                if let Some(t) = &mut self.table {
                    t.row = i.min(n.saturating_sub(1));
                }
            }
            Pane::Right => self.annot_sel = i.min(self.annots.len().saturating_sub(1)),
        }
    }

    fn open_pdf(&mut self) {
        let Some(&i) = self.visible.get(self.sel) else { return };
        let Some(pdf) = self.pdf_for(i, Retry::OpenPdf) else { return };
        let Some(pdf) = pdf else { return self.status = "no pdf".into() };
        let path = match self.lib.pdf_path(&pdf) {
            Ok(p) => p,
            Err(e) => return self.status = e.to_string(),
        };
        if let Some(remote) = self.lib.remote_pdf(&pdf) {
            let account = yeda_secrets::webdav_account(&remote.dav.user, &remote.dav.url);
            let label = format!("{}@{}", remote.dav.user, remote.dav.url);
            let file = remote.path.clone();
            if let Some(password) = self.secret(&account, &label, Retry::OpenPdf) {
                self.status = format!("checking {file}…");
                self.job = Some(Job::Download(pdf.clone(), password));
            }
            return;
        }
        if !path.exists() {
            return self.status = format!("missing: {}", path.display());
        }
        self.launch_reader(&path, &pdf);
    }

    /// Opens the file `path` of the selected reference's `pdf`, with its annotations folder.
    fn launch_reader(&mut self, path: &std::path::Path, pdf: &Pdf) {
        let Some(key) = self.selected().map(|r| r.key.clone()) else { return };
        let mut words = self.lib.reader.as_deref().filter(|r| *r != "default").unwrap_or_default().split_whitespace();
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
            .arg(path)
            .arg("--annotations")
            .arg(self.path(&library::pdf_annotations_dir(&key, pdf)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Ok(child) => reap(child),
            Err(e) => self.status = format!("{}: {e}", command.get_program().to_string_lossy()),
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
    fn close_graph(&mut self) {
        self.graph = None;
        if self.right == RightView::Graph {
            self.back_to_details();
        }
    }

    fn in_graph(&self) -> bool {
        self.pane == Pane::Right && self.right == RightView::Graph && self.graph.is_some()
    }

    /// The OpenAlex key known this session. The secret store is only asked once the keyless
    /// budget runs out, since asking can start a provider with a desktop-wide prompt.
    fn openalex_key(&mut self) -> Option<String> {
        self.secrets.get(OPENALEX_ACCOUNT).cloned().filter(|k| !k.is_empty())
    }

    /// Graphs the selected or marked references; `args` may be `key` or `close`; `refresh` fetches citing works again.
    fn graph_command(&mut self, args: &str, refresh: bool, command: &str) {
        if self.graph_job.is_some() {
            self.status = "a graph is already being fetched".into();
            return;
        }
        let (scope, seeds): (&str, Vec<usize>) = match args.split_whitespace().collect::<Vec<_>>()[..] {
            ["close"] if self.right == RightView::Clusters && self.cluster.is_some() => return self.close_cluster(),
            ["close"] => {
                self.close_cluster();
                return self.close_graph();
            }
            ["tags"] => return self.open_cluster(None),
            ["col", column] => return self.open_cluster(Some(column)),
            [] | ["citations"] => ("the selection", self.targets()),
            _ => {
                self.status = "usage: :graph[!] [citations], :graph tags, :graph col <column>, :graph close".into();
                return;
            }
        };
        if seeds.is_empty() {
            self.status = "no reference selected".into();
            return;
        }
        let seeds = seeds.iter().map(|&i| self.lib.refs[i].key.clone()).collect();
        self.start_graph(seeds, Vec::new(), Vec::new(), refresh, command, scope, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn start_graph(&mut self, seeds: Vec<String>, explored: Vec<String>, opened: Vec<String>, refresh: bool, command: &str, scope: &str, focus: Option<String>) {
        let key = self.openalex_key();
        let keyless = key.is_none();
        let rx = citations::spawn(citations::Request {
            refs: self.lib.refs.clone(),
            seeds: seeds.clone(),
            settings: self.lib.graph.clone(),
            cache: self.lib.cache.clone().unwrap_or_else(std::env::temp_dir),
            key,
            refresh,
            explored,
            opened,
        });
        self.status = "graph: starting…".into();
        self.graph_job = Some(GraphJob { rx, command: command.into(), scope: scope.into(), keyless, seeds, focus });
    }

    /// Applies a finished graph fetch; called from the event loop.
    pub fn poll_graph(&mut self) {
        let Some(job) = &self.graph_job else { return };
        loop {
            match job.rx.try_recv() {
                Ok(citations::Msg::Progress(s)) => {
                    self.status = format!("graph: {s}");
                    self.log.write(&self.status);
                }
                Ok(citations::Msg::Done(result)) => {
                    let job = self.graph_job.take().unwrap();
                    return self.finish_graph(result, job);
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

    fn finish_graph(&mut self, result: Result<citations::Fetched, String>, job: GraphJob) {
        let scope = job.scope;
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
        let mut graph = f.graph;
        let (tab, by_year, ego) = self.graph.as_ref().map_or((GraphTab::Cites, false, false), |g| (g.tab, g.by_year, g.ego));
        // A rebuild after opening a paper keeps the exploration; a new :graph starts over.
        let old = self.graph.take().filter(|_| job.focus.is_some());
        let (opened, explored, back) = old.as_ref().map_or_else(Default::default, |g| (g.opened.clone(), g.explored.clone(), g.back.clone()));
        let keys = self.lib.refs.iter().filter_map(|r| Some((graph::work_id(r)?.to_string(), r.key.clone()))).collect();
        for (id, lists) in &f.lists {
            if let Some(i) = graph.find(id) {
                graph.extend(i, &lists.cites, &lists.cited_by, &keys);
            }
        }
        for (id, list) in &f.cocited {
            if let Some(i) = graph.find(id) {
                graph.add_cocited(i, list, &keys);
            }
        }
        graph.relayout();
        self.status = format!("graph of {scope}: {} papers, {} citations{unknown}", graph.nodes.iter().filter(|n| !n.hidden).count(), graph.edges.len());
        for n in graph.nodes.iter_mut() {
            n.explored = explored.contains(&n.id);
        }
        let sel = match &job.focus {
            Some(id) => graph.find(id),
            None => self.selected().and_then(|r| graph.find_key(&r.key)),
        };
        self.graph = Some(GraphView {
            graph,
            sel: sel.unwrap_or(0),
            nbr: 0,
            scope,
            tab,
            by_year,
            back,
            ego,
            scroll: 0,
            scroll_for: None,
            seeds: job.seeds,
            opened,
            explored,
        });
        self.right = RightView::Graph;
    }

    /// Selects the list's reference in the graph, if it is there.
    fn follow_in_graph(&mut self) {
        let Some(key) = self.selected().map(|r| r.key.clone()) else { return };
        if let Some(c) = &mut self.cluster
            && let Some(i) = c.clusters.find_key(&key)
            && i != c.sel
        {
            c.sel = i;
            c.nbr = 0;
        }
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

    /// Opens the picked paper: the first time, it becomes a seed (if it isn't one) and the graph is
    /// rebuilt with it and its full reference and citing lists.
    fn graph_go(&mut self) {
        let Some(g) = &mut self.graph else { return };
        let Some(&(_, target)) = g.entries().get(g.nbr) else { return };
        let node = &g.graph.nodes[target];
        let id = node.id.clone();
        g.back.push(g.graph.nodes[g.sel].id.clone());
        if g.opened.contains(&id) || self.graph_job.is_some() {
            return self.graph_select(target);
        }
        g.opened.push(id.clone());
        if !node.seed {
            g.explored.push(id.clone());
        }
        let label = format!("{} {}", node.first_author, node.year.map(|y| y.to_string()).unwrap_or_default());
        let (seeds, explored, opened, scope) = (g.seeds.clone(), g.explored.clone(), g.opened.clone(), g.scope.clone());
        self.graph_select(target);
        self.start_graph(seeds, explored, opened, false, "", &scope, Some(id));
        self.status = format!("graph: opening {label}…");
    }

    /// Moves the reference list to the selected paper, so the details and other commands follow.
    fn cluster_follow_list(&mut self) {
        let Some(c) = &self.cluster else { return };
        if let yeda_core::clusters::Kind::Paper(key) = &c.clusters.nodes[c.sel].kind
            && let Some(pos) = self.visible.iter().position(|&r| self.lib.refs[r].key == *key)
        {
            self.sel = pos;
        }
    }

    /// `:graph tags` (`column` None) or `:graph col <column>`, of the selected or marked references.
    fn open_cluster(&mut self, column: Option<&str>) {
        let refs: Vec<&Ref> = self.targets().iter().map(|&i| &self.lib.refs[i]).collect();
        if refs.is_empty() {
            return self.status = "no reference selected".into();
        }
        let clusters = match column {
            None => yeda_core::clusters::Clusters::build(refs, |r| r.tags.clone()),
            Some(column) => {
                let Some(table) = self.lib.tables.iter().find(|(_, t)| t.columns.iter().any(|c| c == column)).map(|(n, _)| n.clone()) else {
                    let all: Vec<&str> = self.lib.tables.values().flat_map(|t| t.columns.iter().map(String::as_str)).collect();
                    return self.status = format!("no table has a column '{column}' (columns: {})", all.join(", "));
                };
                yeda_core::clusters::Clusters::build(refs, |r| r.tables.get(&table).and_then(|row| row.get(column)).cloned().into_iter().collect())
            }
        };
        if clusters.nodes.is_empty() {
            return self.status = format!("none of these references has {}", column.map_or("tags".to_string(), |c| format!("a value for '{c}'")));
        }
        self.cluster = Some(ClusterView::new(clusters, column.unwrap_or("tags").to_string()));
        self.right = RightView::Clusters;
        self.pane = Pane::Right;
        self.follow_in_graph();
    }

    fn close_cluster(&mut self) {
        self.cluster = None;
        if self.right == RightView::Clusters {
            self.back_to_details();
        }
    }

    fn graph_back(&mut self) {
        let Some(g) = &mut self.graph else { return };
        if let Some(prev) = g.back.pop().and_then(|id| g.graph.find(&id)) {
            self.graph_select(prev);
        }
    }

    /// The highlighted neighbour, else the selected node.
    fn graph_target(&self) -> Option<&graph::Node> {
        let g = self.graph.as_ref()?;
        let i = g.entries().get(g.nbr).map_or(g.sel, |&(_, i)| i);
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
        self.xdg_open(format!("https://doi.org/{doi}"));
    }

    fn xdg_open(&mut self, target: impl AsRef<std::ffi::OsStr>) {
        // ponytail: xdg-open only; macOS would need `open`.
        let result = Command::new("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        match result {
            Ok(child) => reap(child),
            Err(e) => self.status = format!("xdg-open: {e}"),
        }
    }

    /// Opens an image annotation in the image viewer, any other in $EDITOR at its entry.
    fn open_annotation(&mut self) {
        let (Some(key), Some((pdf, a))) = (self.annots_key.clone(), self.annots.get(self.annot_sel)) else { return };
        if let Some(img) = &a.image {
            let path = self.path(&library::pdf_annotations_dir(&key, pdf)).join(img);
            return self.xdg_open(path);
        }
        let path = self.path(&annotations::file_path(&key, pdf));
        let entry = format!("id = \"{}\"", a.id);
        let line = fs::read_to_string(&path).ok().and_then(|t| t.lines().position(|l| l.trim() == entry)).map(|i| i + 1);
        self.edit = Some((path, line));
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
        if !self.confirm_many("fetch PDFs for", targets.iter().filter(|&&i| self.lib.refs[i].pdfs.is_empty()).count(), command) {
            return;
        }
        let items: Vec<fetch::Item> = targets
            .iter()
            .map(|&i| &self.lib.refs[i])
            .filter(|r| r.pdfs.is_empty())
            .map(|r| fetch::Item { key: r.key.clone(), doi: graph::lookup_doi(r), openalex: graph::work_id(r).map(String::from), file: fulltext::file_name(r) })
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
        let openalex_key = self.openalex_key();
        let request = fetch::Request { items, settings: self.lib.fulltext.clone(), downloads, openalex_key };
        self.status = "fetch: starting…".into();
        self.fetch_job = Some(FetchJob { rx: fetch::spawn(request), step: self.step.clone(), location, password, added: vec![], failed: vec![] });
    }

    /// Attaches fetched PDFs as they arrive; called from the event loop.
    pub fn poll_fetch(&mut self) {
        let Some(mut job) = self.fetch_job.take() else { return };
        loop {
            match job.rx.try_recv() {
                Ok(fetch::Msg::Progress(s)) => {
                    self.log.write(&s);
                    self.status = s;
                }
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
            if let (Some(max), Ok(pages)) = (fulltext::max_pages(&self.lib.refs[i]), poppler::page_count(file)) {
                anyhow::ensure!(
                    pages <= max,
                    "the downloaded PDF has {pages} pages, more than this paper should (a whole issue?); if it's right, :attach {}",
                    file.display()
                );
            }
        }
        let r = self.attach_file(i, file, &job.location, job.password.as_deref())?;
        self.write(&job.step, [self.lib.save(&r)?])?;
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

fn unreadable(lib: &Library) -> String {
    match lib.unreadable.as_slice() {
        [] => String::new(),
        [(_, e)] => format!("left out a reference whose file doesn't parse: {e}"),
        [(_, e), ..] => format!("left out {} references whose files don't parse; the first: {e}", lib.unreadable.len()),
    }
}

/// Waits for `child` in the background, so it doesn't stay a zombie until yeda exits.
fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || child.wait());
}

#[cfg(test)]
mod tests {
    #[test]
    fn edits_at_the_cursor() {
        use ratatui::crossterm::event::KeyCode::*;
        let (mut buf, mut back) = ("tag ml".to_string(), 0);
        let mut key = |code, ctrl| assert!(super::edit_line(&mut buf, &mut back, code, ctrl));
        key(Left, false);
        key(Left, false);
        key(Char('x'), false);
        key(Backspace, false);
        key(Backspace, false);
        key(Char('é'), false);
        key(Delete, false);
        key(Char('w'), true);
        key(End, false);
        key(Char('!'), false);
        assert_eq!((buf.as_str(), back), ("l!", 0));
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(super::unquote("\"~/a b.pdf\""), "~/a b.pdf");
        assert_eq!(super::unquote("'a.pdf'"), "a.pdf");
        assert_eq!(super::unquote("~/a b.pdf"), "~/a b.pdf");
        assert_eq!(super::unquote("\"a.pdf'"), "\"a.pdf'");
    }
}
