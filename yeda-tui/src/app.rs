use crate::{citations, fetch, ops, pdftext, undo};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use crate::keys::{self, Keymap, Lookup};
use ratatui_image::picker::{Capability, Picker};
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
use yeda_core::graph::{self, Graph};
use yeda_core::search::{self, FILTER_FIELDS, Searcher};
use crate::cluster::ClusterView;
use crate::complete::Completion;
use crate::discover::Candidate;
use crate::setup::{self, SetupForm};
use crate::table::{self, TableView};
use yeda_core::tables::{self, Row};
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
    /// The `:envvars` list.
    Envvars,
    Filter,
    /// The `:setup` form.
    Setup,
    /// The `:setup` list of everything it sets.
    SetupMenu,
    /// The existing entries of one `:setup` kind, to edit or to add to.
    SetupPick,
    /// `:setup web-search`: the provider groups and the discover limits.
    WebSearch,
    /// `:discover` without arguments: what to search from and which LLM to ask.
    Discover,
    /// The papers `:discover` found.
    Found,
    /// The plan `:do` made, waiting to be approved.
    Plan,
    /// git's output from `:sync` or `:git-init`.
    Git,
    /// `:diff`: the changes since the last commit.
    Diff,
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

pub use crate::keydoc::{ACTIONS, COMMANDS};

/// Actions that change every marked reference, asked about first: action, and what it does.
const CONFIRM_MANY: [(&str, &str); 5] = [
    ("status-next", "change the reading status of"),
    ("status-prev", "change the reading status of"),
    ("tags", "change the tags of"),
    ("move", "move"),
    ("extract", "extract annotations into"),
];

/// The `f` popup: one value per `search::FILTER_FIELDS` entry, then free text.
/// The `:setup` connection test running; saved when it passes. The thread owns the secret or the
/// sign-in's credentials and sends them on success.
struct SetupJob {
    rx: Receiver<anyhow::Result<Option<String>>>,
    section: toml::Table,
    setup: setup::Setup,
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
/// Moves a provider's name in `[providers]` and in every `[llm.*]` profile that uses it.
fn rename_provider(root: &mut toml::Table, old: &str, new: &str) {
    if let Some(providers) = root.get_mut("providers").and_then(|v| v.as_table_mut()) {
        providers.remove(old);
    }
    let profiles = root.get_mut("llm").and_then(|v| v.as_table_mut());
    for profile in profiles.into_iter().flat_map(|t| t.iter_mut()).filter_map(|(_, v)| v.as_table_mut()) {
        if profile.get("provider").and_then(|v| v.as_str()) == Some(old) {
            profile.insert("provider".into(), new.into());
        }
    }
}

pub(crate) fn edit_line(buf: &mut String, back: &mut usize, code: KeyCode, ctrl: bool) -> bool {
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

pub struct DiffView {
    pub changes: Vec<crate::sync::Change>,
    pub sel: usize,
    pub marked: BTreeSet<usize>,
    /// The selected file's diff.
    pub text: String,
    pub scroll: u16,
    /// The first `d` of `d d`.
    d: bool,
    /// Asking y/n before undoing.
    confirming: bool,
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

struct TextJob {
    rx: Receiver<pdftext::Extracted>,
    total: usize,
    done: usize,
    error: Option<String>,
}

/// `:cache`: each PDF's secret-store account and server path, with how its download went.
struct CacheJob {
    rx: Receiver<(String, String, anyhow::Result<()>)>,
    total: usize,
    done: usize,
    failed: usize,
    error: Option<String>,
    rejected: bool,
}

/// `:storage migrate`: the moved references, once every PDF is copied.
struct MigrateJob {
    rx: Receiver<Result<(Vec<Ref>, usize), String>>,
    from: String,
    to: String,
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

#[derive(Clone)]
pub struct Batch {
    step: undo::Step,
    /// The collection new references go into.
    collection: Option<String>,
    /// Whether references already in the library join `collection` too (`:add --to`).
    join_existing: bool,
    /// Mark what the batch added or found, so the next command of a `:do` plan acts on it.
    mark_result: bool,
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

/// One row of the picker: the entry's name, how it is set, and whether Enter adds it.
pub struct SetupOption {
    pub name: String,
    pub detail: String,
    pub new: bool,
}

pub struct SetupPicker {
    pub kind: setup::Kind,
    pub options: Vec<SetupOption>,
    pub sel: usize,
}

/// A fuzzy dropdown over one `:setup` field. A pick writes into the field; typing still works.
pub struct Combo {
    pub choice: setup::Choice,
    pub field: usize,
    /// The field's value when it was focused; Esc puts it back.
    original: String,
    /// The typed filter. Empty on focus, so every option is visible at first.
    query: String,
    last_text: String,
    pub base: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
    pub sel: usize,
    pub open: bool,
    /// Loading or a refused model list, shown without blocking the form.
    pub note: Option<String>,
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
    /// PDFs whose text `searcher` has, or is being extracted.
    text_indexed: HashSet<PathBuf>,
    text_job: Option<TextJob>,
    cache_job: Option<CacheJob>,
    migrate_job: Option<MigrateJob>,
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
    pub diff: Option<DiffView>,
    pub job: Option<Job>,
    picker: Picker,
    /// The background of alternate rows, once the terminal reports its own.
    pub stripe: Option<Color>,
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
    discover_job: Option<Receiver<crate::discover::Msg>>,
    assist_job: Option<Receiver<Result<String, String>>>,
    /// The plan `:do` made, shown until approved or cancelled.
    pub plan: Option<yeda_core::assist::Plan>,
    /// Commands of an approved plan still to run, each once the one before is done.
    plan_queue: std::collections::VecDeque<String>,
    /// Steps in the approved plan, and whether it has not finished yet; for the log.
    plan_total: usize,
    assist_request: String,
    plan_running: bool,
    /// How the last :discover answered, if the user should know (no web search).
    pub found_note: Option<String>,
    pub found: Vec<Candidate>,
    pub found_sel: usize,
    pub found_marked: BTreeSet<usize>,
    /// The `:setup` list's rows, from `setup::overview`; empty when it is closed.
    pub setup_menu: Vec<[String; 3]>,
    pub setup_menu_sel: usize,
    /// The list of existing providers, LLM profiles, storage locations or tables, while it is open or its form is.
    pub setup_picker: Option<SetupPicker>,
    /// The `:setup web-search` popup, staged until Enter.
    pub web_search: Option<crate::websearch::WebSearchForm>,
    pub discover_form: Option<crate::discoverform::DiscoverForm>,
    /// A setup form opened from `:setup web-search`, which it returns to.
    setup_from_web_search: bool,
    setup_job: Option<SetupJob>,
    /// The open dropdown on the selected `:setup` field.
    pub combo: Option<Combo>,
    model_job: Option<Receiver<Result<Vec<yeda_net::llm::Model>, String>>>,
    /// Provider and account the model list was fetched for; cleared when either changes.
    models_for: Option<String>,
    pub help_scroll: u16,
    /// What `/` searches for in a help page, and whether it's still being typed.
    pub help_query: String,
    pub help_typing: bool,
    /// The line of the match `n`/`N` last went to, until another key is pressed.
    pub help_hit: Option<usize>,
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
        let stripe = picker.capabilities().iter().find_map(|c| match c {
            Capability::Background(r, g, b) => Some(crate::ui::alternate((*r, *g, *b))),
            _ => None,
        });
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
            text_indexed: HashSet::new(),
            text_job: None,
            cache_job: None,
            migrate_job: None,
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
            diff: None,
            job: None,
            picker,
            stripe,
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
            assist_job: None,
            plan: None,
            plan_queue: Default::default(),
            plan_total: 0,
            assist_request: String::new(),
            plan_running: false,
            discover_form: None,
            found_note: None,
            found: Vec::new(),
            found_sel: 0,
            found_marked: BTreeSet::new(),
            setup_menu: Vec::new(),
            setup_menu_sel: 0,
            setup_picker: None,
            web_search: None,
            setup_from_web_search: false,            setup_job: None,
            combo: None,
            model_job: None,
            models_for: None,
            help_scroll: 0,
            help_query: String::new(),
            help_hit: None,
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
        for warning in app.lib.warnings.clone() {
            app.log.write(&warning);
        }
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
        if self.text_job.is_none() && self.searches_text() {
            self.index_text();
        }
    }

    fn searches_text(&self) -> bool {
        let saved = match self.current_filter() {
            Filter::Search(_, q) => q.as_str(),
            _ => "",
        };
        [self.query.as_str(), saved].iter().any(|q| search::parse(q).fields.iter().any(|(name, _)| name == "text"))
    }

    /// Starts extracting the text of every PDF on this machine not indexed yet, for `text:` terms.
    fn index_text(&mut self) {
        let pdfs: Vec<(String, PathBuf)> = self
            .lib
            .refs
            .iter()
            .flat_map(|r| r.pdfs.iter().filter_map(|p| Some((r.key.clone(), self.lib.pdf_path(p).ok()?))))
            .filter(|(_, path)| path.is_file() && !self.text_indexed.contains(path))
            .collect();
        if pdfs.is_empty() {
            return;
        }
        self.text_indexed.extend(pdfs.iter().map(|(_, p)| p.clone()));
        self.status = format!("indexing the text of {} PDFs…", pdfs.len());
        self.text_job = Some(TextJob { total: pdfs.len(), rx: pdftext::spawn(pdfs, self.lib.cache.clone()), done: 0, error: None });
    }

    pub fn poll_text(&mut self) {
        let Some(job) = &mut self.text_job else { return };
        let before = job.done;
        let finished = loop {
            match job.rx.try_recv() {
                Ok((key, text)) => {
                    job.done += 1;
                    match text {
                        Ok(text) => self.searcher.add_text(&key, &text),
                        Err(e) => job.error = job.error.take().or(Some(e)),
                    }
                }
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        if job.done == before && !finished {
            return;
        }
        self.status = match (finished, &job.error) {
            (false, _) => format!("indexing PDF text: {}/{}…", job.done, job.total),
            (true, None) => format!("indexed the text of {} PDFs", job.total),
            (true, Some(e)) => format!("indexed the text of {} PDFs; some failed: {e}", job.total),
        };
        if finished {
            self.text_job = None;
        }
        let sel = self.selected().map(|r| r.key.clone());
        self.refilter();
        if let Some(pos) = sel.and_then(|key| self.visible.iter().position(|&i| self.lib.refs[i].key == key)) {
            self.sel = pos;
        }
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
            Mode::Envvars => self.envvars_key(k),
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
            Mode::SetupMenu => self.setup_menu_key(k),
            Mode::SetupPick => self.setup_pick_key(k),
            Mode::WebSearch => self.websearch_key(k),
            Mode::Discover => self.discover_form_key(k),
            Mode::Found => self.found_key(k),
            Mode::Plan => self.plan_key(k),
            Mode::Git => self.git_key(k),
            Mode::Diff => self.diff_key(k),
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

    /// Sorts and the selected library entry stay.
    fn clear_all_searches(&mut self) {
        self.side_query.clear();
        self.query.clear();
        if let Some(t) = &mut self.table {
            t.query.clear();
            t.filters.clear();
        }
        self.refilter_sidebar(false);
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
            "table-clear" => {
                let table_search = self.table.as_ref().is_some_and(|t| !t.query.is_empty() || !t.filters.is_empty());
                match &mut self.table {
                    Some(t) if t.open => t.open = false,
                    Some(t) if table_search => {
                        t.query.clear();
                        t.filters.clear();
                    }
                    _ if !self.side_query.is_empty() || !self.query.is_empty() => self.clear_all_searches(),
                    _ => self.close_table(),
                }
            }
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
                let sidebar = self.pane == Pane::Sidebar;
                let own_search = if sidebar { !self.side_query.is_empty() } else { !self.query.is_empty() };
                if let Some((_, before)) = self.visual.take() {
                    self.marked = before;
                } else if self.pane == Pane::List && !self.marked.is_empty() {
                    self.status = format!("unmarked {}", self.marked.len());
                    self.marked.clear();
                } else if !own_search {
                    self.clear_all_searches();
                } else if sidebar {
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
                _ if self.pane == Pane::Sidebar => self.status = "select a saved search in the library column to duplicate it".into(),
                _ => match self.selected() {
                    Some(r) => {
                        self.setup = Some(SetupForm::new(setup::Kind::Reference, &r.key.clone(), &self.lib));
                        self.mode = Mode::Setup;
                    }
                    None => self.status = "no reference selected".into(),
                },
            },
            "note" => self.edit = self.selected().map(|r| (self.path(&library::note_path(&r.key)), None)),
            "tags" => self.open_chooser(PickKind::Tags),
            "move" => self.open_chooser(PickKind::Move),
            "command" => self.mode = Mode::Prompt(Prompt::Command),
            "close-or-quit" if let Some(t) = self.table.as_mut().filter(|t| t.open && self.right == RightView::Tables) => t.open = false,
            "close-or-quit" if self.right == RightView::Tables && self.table.is_some() => self.close_table(),
            "close-or-quit" if self.right == RightView::Clusters && self.cluster.is_some() => self.close_cluster(),
            "close-or-quit" if self.graph.is_some() => self.close_graph(),
            "close-or-quit" if self.right == RightView::Annotations => self.back_to_details(),
            "close-or-quit" => self.quit = true,
            "add" if args.is_empty() => self.mode = Mode::Prompt(Prompt::Add),
            "add" => match options(args, &["to"], &[]) {
                Ok((opts, rest)) => match opts.get("to").and_then(|v| v.last()) {
                    Some(to) => self.add_to(to, &rest),
                    None => self.submit(Prompt::Add, args),
                },
                Err(e) => self.status = format!("{e:#}"),
            },
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
        if let Some(form) = &mut self.setup {
            form.sync_columns();
        }
        self.cursor_back = 0;
        if self.mode == Mode::Setup {
            self.sync_combo();
        }
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
                        self.log.command(text.trim());
                    }
                    self.submit(p, text.trim());
                }
            }
            _ => {}
        }
        match mode {
            Mode::Search => self.refilter(),
            Mode::SideSearch => self.refilter_sidebar(true),
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

    /// Keeps the dropdown on the selected field, opening it for known values and fetching the
    /// provider's model list once per provider and account.
    fn sync_combo(&mut self) {
        let (field, choice, text) = match &self.setup {
            Some(form) => match form.choice(form.sel) {
                Some(choice) => (form.sel, choice, form.values[form.sel].clone()),
                None => {
                    self.combo = None;
                    return;
                }
            },
            None => {
                self.combo = None;
                self.model_job = None;
                self.models_for = None;
                return;
            }
        };
        let base = match choice {
            setup::Choice::Models => self
                .combo
                .as_ref()
                .filter(|c| c.field == field && c.choice == choice)
                .map(|c| c.base.clone())
                .unwrap_or_default(),
            other => setup::choices(other, &self.lib),
        };
        let mut fresh = false;
        match &mut self.combo {
            Some(c) if c.field == field && c.choice == choice => {
                if c.last_text != text {
                    if c.choice == setup::Choice::BoundProviders {
                        self.model_job = None;
                        self.models_for = None;
                    }
                    c.last_text = text.clone();
                    c.query = text;
                    c.sel = 0;
                }
            }
            _ => {
                self.combo = Some(Combo {
                    choice,
                    field,
                    original: text.clone(),
                    query: String::new(),
                    last_text: text,
                    base,
                    options: Vec::new(),
                    sel: 0,
                    open: true,
                    note: None,
                });
                fresh = true;
            }
        }
        self.refresh_combo();
        if fresh
            && let Some(c) = &mut self.combo
        {
            let original = c.original.clone();
            c.sel = c.options.iter().position(|(_, value)| value == &original).unwrap_or(0);
        }
        if choice == setup::Choice::Models {
            self.ensure_models();
        }
    }

    fn refresh_combo(&mut self) {
        let Some(c) = &self.combo else { return };
        let query = c.query.trim().to_string();
        let base = c.base.clone();
        let searcher = &mut self.searcher;
        let mut scored: Vec<(u32, String, String)> = base
            .iter()
            .filter_map(|(label, value)| {
                let score = if query.is_empty() { Some(0) } else { searcher.score(&query, label).or_else(|| searcher.score(&query, value)) }?;
                Some((score, label.clone(), value.clone()))
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        let mut options: Vec<(String, String)> = scored.into_iter().map(|(_, label, value)| (label, value)).collect();
        if !query.is_empty() && !options.iter().any(|(_, value)| value == &query) {
            options.push((format!("+ use \"{query}\""), query));
        }
        let Some(c) = &mut self.combo else { return };
        c.options = options;
        c.sel = c.sel.min(c.options.len().saturating_sub(1));
    }

    /// Resolves the provider's credential the same way the connection test does, then asks for
    /// its model list in the background. Errors stay in the dropdown; typing is never blocked.
    fn ensure_models(&mut self) {
        let Some(form) = &self.setup else { return };
        let name = form.values[0].trim().to_string();
        let provider = form.values[1].trim().to_string();
        let providers = self.lib.providers.clone();
        let account = providers.get(&provider).map(|p| yeda_secrets::provider_account(&provider, p.url())).unwrap_or_default();
        let key = format!("{provider}:{account}");
        if self.model_job.is_some() || self.models_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let profile = yeda_core::llm::Profile { provider: provider.clone(), model: String::new() };
        let found = setup::profile_credential(&providers, &name, &profile, |account| match self.secrets.get(account) {
            Some(secret) => Ok(Some(secret.clone())),
            None => yeda_secrets::get_as(account, self.alias_of(account).as_deref()),
        });
        let (bound, secret) = match found {
            Ok(found) => found,
            Err(e) => {
                if let Some(c) = &mut self.combo {
                    c.note = Some(format!("{e:#}"));
                }
                self.models_for = Some(key);
                return;
            }
        };
        let secret = self.renewed_codex(&bound, account, secret);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(yeda_net::llm::models(&bound, secret.as_deref()).map_err(|e| e.to_string()));
        });
        self.model_job = Some(rx);
        self.models_for = Some(key);
        if let Some(c) = &mut self.combo {
            c.note = Some("loading models…".into());
        }
    }

    fn poll_models(&mut self) {
        let result = {
            let Some(rx) = &self.model_job else { return };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("the model list request stopped".into())),
            }
        };
        let Some(result) = result else { return };
        self.model_job = None;
        if !self.combo.as_ref().is_some_and(|c| c.choice == setup::Choice::Models) {
            return;
        }
        let note = match result {
            Ok(models) => {
                if let Some(c) = &mut self.combo {
                    c.base = models.into_iter().map(|m| (m.label, m.id)).collect();
                }
                None
            }
            Err(e) => Some(format!("model list: {e}")),
        };
        if let Some(c) = &mut self.combo {
            c.note = note;
        }
        self.refresh_combo();
    }

    fn combo_step(&mut self, delta: isize) {
        let Some(c) = &mut self.combo else { return };
        if c.options.is_empty() {
            return;
        }
        let n = c.options.len() as isize;
        c.sel = (c.sel as isize + delta).rem_euclid(n) as usize;
    }

    fn combo_pick(&mut self) {
        let Some((field, value)) = self
            .combo
            .as_ref()
            .and_then(|c| c.options.get(c.sel).cloned().map(|(_, v)| (c.field, v)))
        else {
            return;
        };
        if self.combo.as_ref().is_some_and(|c| c.choice == setup::Choice::BoundProviders) {
            self.model_job = None;
            self.models_for = None;
        }
        if let Some(c) = &mut self.combo {
            c.original = value.clone();
            c.last_text = value.clone();
            c.query.clear();
            c.open = false;
            c.sel = 0;
        }
        if let Some(form) = &mut self.setup {
            form.values[field] = value;
            if form.kind == setup::Kind::Provider && field == 1 {
                form.provider_changed();
            }
            if form.kind == setup::Kind::Storage && field == 1 {
                form.storage_kind_changed();
            }
        }
        self.cursor_back = 0;
    }

    /// While the dropdown is open, navigation belongs to it; editing keys still reach the form.
    fn combo_key(&mut self, k: KeyEvent, ctrl: bool) -> bool {
        if !self.combo.as_ref().is_some_and(|c| c.open) {
            return false;
        }
        match k.code {
            KeyCode::Esc => {
                let Some((field, original)) = self.combo.as_ref().map(|c| (c.field, c.original.clone())) else { return false };
                if let Some(c) = &mut self.combo {
                    c.open = false;
                    c.query.clear();
                    c.last_text = original.clone();
                }
                if let Some(form) = &mut self.setup {
                    form.values[field] = original;
                }
                self.cursor_back = 0;
                self.refresh_combo();
                true
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.combo_step(-1);
                true
            }
            KeyCode::Down | KeyCode::Tab => {
                self.combo_step(1);
                true
            }
            KeyCode::Char('p') if ctrl => {
                self.combo_step(-1);
                true
            }
            KeyCode::Char('n') if ctrl => {
                self.combo_step(1);
                true
            }
            KeyCode::Enter => {
                self.combo_pick();
                true
            }
            _ => false,
        }
    }

    fn setup_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Waiting for the browser or a slow test: c copies the sign-in link, Esc cancels and
        // drops the thread's result. Both end here, before the form is borrowed for editing.
        if self.setup.as_ref().is_some_and(|form| form.testing) {
            let link = self.setup.as_ref().and_then(|form| form.sign_in_url.clone());
            if k.code == KeyCode::Esc {
                self.setup_job = None;
                self.close_setup();
            } else if link.is_some() && k.code == KeyCode::Char('c') {
                self.status = match self.copy(link.as_ref().unwrap()) {
                    Ok(()) => "sign-in link copied to the clipboard".into(),
                    Err(e) => format!("clipboard: {e}"),
                };
            }
            return;
        }
        if let Some(form) = self.setup.as_mut().filter(|form| form.confirm_move) {
            form.confirm_move = false;
            if k.code == KeyCode::Char('y') {
                form.moving_confirmed = true;
                return self.setup_key(KeyEvent::from(KeyCode::Enter));
            }
            self.status = "the key stays where it is; change the URL back or type a key".into();
            return;
        }
        self.sync_combo();
        if self.combo_key(k, ctrl) {
            return;
        }
        let clear_on_type = self.combo.as_ref().is_some_and(|c| c.open)
            && matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete)
            && self
                .setup
                .as_ref()
                .zip(self.combo.as_ref())
                .is_some_and(|(form, c)| form.values[form.sel] == c.original);
        let Some(form) = &mut self.setup else { return };
        let n = form.values.len();
        match k.code {
            KeyCode::Esc => self.close_setup(),
            KeyCode::Enter => match form.section() {
                Ok((_, setup::Setup::Search(saved))) => {
                    let (name, old) = (form.name().to_string(), form.old.clone());
                    let result = self.lib.save_search(&name, &saved, old.as_deref().filter(|o| *o != name));
                    self.after_config_change(result);
                    self.close_setup();
                    // Applies the new sort even if only the sort changed.
                    self.shown_filter = Filter::All;
                    self.select_filter(&Filter::Search(name.clone(), saved.query));
                    self.status = format!("saved search '{name}'");
                }
                Ok((_, setup::Setup::Reference(r))) if let Some(x) = r.doi.as_deref().and_then(|d| self.lib.find_doi(d)) => {
                    self.status = format!("{} has this DOI already", x.key);
                }
                Ok((_, setup::Setup::Reference(mut r))) => {
                    match form.old.as_deref().and_then(|k| self.lib.get(k)) {
                        Some(from) => {
                            (r.tags, r.collections, r.status) = (from.tags.clone(), from.collections.clone(), from.status.clone());
                            // The Zotero key belongs to the original.
                            let kept = from.extra.iter().filter(|(k, _)| !setup::EXTRAS.contains(&k.as_str()) && *k != "zotero_key");
                            r.extra.extend(kept.map(|(k, v)| (k.clone(), v.clone())));
                        }
                        None => r.collections = match self.current_filter() {
                            Filter::Collection(c) => vec![c.clone()],
                            _ => Vec::new(),
                        },
                    }
                    r.key = self.lib.unique_key(&r);
                    if self.apply(self.lib.save(&r).map(|e| vec![e])) {
                        self.close_setup();
                        self.reload();
                        self.refilter();
                        if let Some(pos) = self.visible.iter().position(|&i| self.lib.refs[i].key == r.key) {
                            self.sel = pos;
                        }
                        self.status = format!("added {}", r.key);
                    }
                }
                Ok((section, setup::Setup::Untested)) => self.save_setup(section, ""),
                Ok((section, s @ setup::Setup::Llm(..))) => {
                    let (name, p) = match &s {
                        setup::Setup::Llm(name, p) => (name.clone(), p.clone()),
                        _ => unreachable!(),
                    };
                    let providers = self.lib.providers.clone();
                    let found = setup::profile_credential(&providers, &name, &p, |account| match self.secrets.get(account) {
                        Some(secret) => Ok(Some(secret.clone())),
                        None => yeda_secrets::get_as(account, self.alias_of(account).as_deref()),
                    });
                    let (bound, key) = match found {
                        Ok(found) => found,
                        Err(e) => {
                            self.status = format!("{e:#}");
                            return;
                        }
                    };
                    let account = yeda_secrets::provider_account(&bound.provider, bound.url());
                    let key = match bound.protocol() {
                        yeda_core::llm::Protocol::OpenaiResponses => self.renewed_codex(&bound, account, key),
                        _ => key,
                    };
                    let (tx, rx) = mpsc::channel();
                    let probe = s.clone();
                    std::thread::spawn(move || {
                        // The profile has no account of its own; its provider's credential stays put.
                        let _ = tx.send(setup::test(&providers, &probe, key.as_deref()).map(|_| None));
                    });
                    if let Some(form) = &mut self.setup {
                        form.testing = true;
                    }
                    self.status = "testing the connection…".into();
                    self.setup_job = Some(SetupJob { rx, section, setup: s });
                }
                Ok((section, setup::Setup::Cloud(name, remote))) => self.submit_cloud(section, name, remote),
                Ok((_, setup::Setup::Local(name, path))) => match anyhow::Context::context(dirs::config_dir(), "no config folder on this device").and_then(|d| ops::save_device_location(&d.join("yeda/config.toml"), &name, &path)) {
                    Ok(()) => {
                        self.close_setup();
                        self.reload();
                        self.status = format!("saved [locations.{name}] to this device's config.toml");
                    }
                    Err(e) => self.status = format!("{e:#}"),
                },
                Ok((_, setup::Setup::Provider(name, _))) if self.lib.providers.contains_key(&name) && form.origin.as_ref().map(|(old, _)| old.as_str()) != Some(name.as_str()) => {
                    self.status = format!("a provider named '{name}' exists already; pick it in :setup provider to edit it");
                }
                Ok((section, setup::Setup::Provider(name, p))) if form.origin.is_some() && form.secret().is_none() && p.auth != yeda_core::llm::Auth::None => self.keep_provider_key(section, name, p),
                Ok((section, setup)) => self.test_and_save_setup(section, setup),
                Err(e) => self.status = format!("{e:#}"),
            },
            KeyCode::Down | KeyCode::Tab => form.sel = (form.sel + 1) % n,
            KeyCode::Up | KeyCode::BackTab => form.sel = (form.sel + n - 1) % n,
            // A secret never goes to a file.
            KeyCode::Char('g') if ctrl && !form.masked(form.sel) => {
                let sel = form.sel;
                self.edit_in_editor(TextTarget::Setup(sel));
            }
            code => {
                if clear_on_type {
                    form.values[form.sel].clear();
                    self.cursor_back = 0;
                }
                edit_line(&mut form.values[form.sel], &mut self.cursor_back, code, ctrl);
            }
        }
        if let Some(form) = &mut self.setup {
            if form.kind == setup::Kind::Provider && form.sel == 1 {
                form.provider_changed();
            }
            if form.kind == setup::Kind::Storage && form.sel == 1 {
                form.storage_kind_changed();
            }
            form.sync_columns();
        }
        self.sync_combo();
    }

    /// Closes the `:setup` form, back to the `:setup` list or the web search popup it came from.
    fn close_setup(&mut self) {
        self.combo = None;
        self.model_job = None;
        self.models_for = None;
        self.setup = None;
        if self.setup_from_web_search {
            self.setup_from_web_search = false;
            if let Some(form) = &mut self.web_search {
                form.refresh();
                self.mode = Mode::WebSearch;
                return;
            }
        }
        self.mode = Mode::Normal;
        if self.setup_picker.is_some() {
            self.refresh_setup_picker();
            self.mode = Mode::SetupPick;
        } else if !self.setup_menu.is_empty() {
            self.open_setup_menu();
        }
    }

    fn open_web_search(&mut self) {
        self.web_search = Some(crate::websearch::WebSearchForm::new(&self.lib));
        self.setup_from_web_search = false;
        self.mode = Mode::WebSearch;
    }

    fn websearch_key(&mut self, k: KeyEvent) {
        let Some(form) = &mut self.web_search else {
            self.mode = Mode::Normal;
            return;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let mut warn = false;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.web_search = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => form.move_cursor(true),
            KeyCode::Char('k') | KeyCode::Up => form.move_cursor(false),
            KeyCode::Char('g') | KeyCode::Home => form.select_edge(false),
            KeyCode::Char('G') | KeyCode::End => form.select_edge(true),
            KeyCode::Char('a') => warn = form.set_group(0),
            KeyCode::Char('f') => warn = form.set_group(1),
            KeyCode::Char('d') if !ctrl => warn = form.set_group(2),
            KeyCode::Left => form.cycle(true),
            KeyCode::Right => form.cycle(false),
            KeyCode::Char('e') => {
                let provider = match form.selected() {
                    Some(crate::websearch::Row::Provider(p)) => p,
                    _ => return,
                };
                let kind = match provider {
                    yeda_core::web_search::Provider::Keenable => setup::Kind::Keenable,
                    yeda_core::web_search::Provider::Exa => setup::Kind::Exa,
                    yeda_core::web_search::Provider::Parallel => setup::Kind::Parallel,
                    yeda_core::web_search::Provider::Firecrawl => setup::Kind::Firecrawl,
                    yeda_core::web_search::Provider::Searxng => setup::Kind::Searxng,
                    yeda_core::web_search::Provider::ModelProviderWebSearch => {
                        self.status = "model_provider_web_search uses the LLM profile's key".into();
                        return;
                    }
                };
                self.setup = Some(SetupForm::new(kind, "", &self.lib));
                self.setup_from_web_search = true;
                self.mode = Mode::Setup;
            }
            KeyCode::Enter => {
                if form.searxng_in_use() && form.settings.searxng_url.is_none() {
                    self.status = "searxng needs an instance URL: press e".into();
                    return;
                }
                let section = form.section();
                let applied = match self.lib.update_config(|t| {
                    let web = config_section(t, "web_search")?;
                    web.extend(section.clone());
                    Ok(())
                }) {
                    Ok(effect) => self.apply(Ok(vec![effect])),
                    Err(e) => {
                        self.reload();
                        self.status = format!("{e:#}");
                        false
                    }
                };
                if !applied {
                    return;
                }
                self.reload();
                self.web_search = None;
                self.mode = Mode::Normal;
                self.status = "saved [web_search] to yeda.toml".into();
            }
            _ => {}
        }
        if warn {
            self.status = "the provider's own search sends your query to the LLM provider and returns an answer yeda cannot check".into();
        }
    }

    fn open_setup_menu(&mut self) {
        self.setup_menu = setup::overview();
        self.setup_menu_sel = self.setup_menu_sel.min(self.setup_menu.len() - 1);
        self.mode = Mode::SetupMenu;
    }

    fn setup_menu_key(&mut self, k: KeyEvent) {
        let last = self.setup_menu.len() - 1;
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.setup_menu_sel = (self.setup_menu_sel + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => self.setup_menu_sel = self.setup_menu_sel.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => self.setup_menu_sel = 0,
            KeyCode::Char('G') | KeyCode::End => self.setup_menu_sel = last,
            KeyCode::Enter => {
                let command = self.setup_menu[self.setup_menu_sel][2].clone();
                self.submit(Prompt::Command, &command);
                if !matches!(self.mode, Mode::Setup | Mode::SetupPick) {
                    self.setup_menu.clear();
                    if self.mode == Mode::SetupMenu {
                        self.mode = Mode::Normal;
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.setup_menu.clear();
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    fn open_setup_picker(&mut self, kind: setup::Kind) {
        self.input.clear();
        self.cursor_back = 0;
        self.setup_picker = Some(SetupPicker { kind, options: Vec::new(), sel: 0 });
        self.refresh_setup_picker();
        self.mode = Mode::SetupPick;
    }

    /// Fuzzy matches of the typed text among the existing entries, then an entry that adds it.
    fn refresh_setup_picker(&mut self) {
        let Some(picker) = &mut self.setup_picker else { return };
        let query = self.input.trim().to_string();
        let mut scored: Vec<(u32, (String, String))> = setup::entries(picker.kind, &self.lib)
            .into_iter()
            .filter_map(|e| if query.is_empty() { Some((0, e)) } else { self.searcher.score(&query, &e.0).map(|s| (s, e)) })
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        let exists = setup::entries(picker.kind, &self.lib).iter().any(|(name, _)| *name == query);
        picker.options = scored.into_iter().map(|(_, (name, detail))| SetupOption { name, detail, new: false }).collect();
        if !query.is_empty() && !exists {
            picker.options.push(SetupOption { detail: String::new(), name: query, new: true });
        }
        picker.sel = picker.sel.min(picker.options.len().saturating_sub(1));
    }

    fn close_setup_picker(&mut self) {
        self.setup_picker = None;
        self.input.clear();
        self.mode = Mode::Normal;
        if !self.setup_menu.is_empty() {
            self.open_setup_menu();
        }
    }

    fn setup_pick_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(picker) = &mut self.setup_picker else {
            self.mode = Mode::Normal;
            return;
        };
        let n = picker.options.len();
        match k.code {
            KeyCode::Esc => self.close_setup_picker(),
            KeyCode::Down | KeyCode::Tab if n > 0 => picker.sel = (picker.sel + 1) % n,
            KeyCode::Char('n') if ctrl && n > 0 => picker.sel = (picker.sel + 1) % n,
            KeyCode::Up | KeyCode::BackTab if n > 0 => picker.sel = (picker.sel + n - 1) % n,
            KeyCode::Char('p') if ctrl && n > 0 => picker.sel = (picker.sel + n - 1) % n,
            KeyCode::Char('*') if self.input.is_empty() => self.make_default(),
            KeyCode::Enter => {
                let Some(option) = picker.options.get(picker.sel) else { return };
                let kind = picker.kind;
                self.setup = Some(SetupForm::new(kind, &option.name.clone(), &self.lib));
                self.mode = Mode::Setup;
                self.input.clear();
            }
            code => {
                if edit_line(&mut self.input, &mut self.cursor_back, code, ctrl) {
                    self.refresh_setup_picker();
                }
            }
        }
    }

    /// Makes the highlighted LLM profile `default_llm`, or the highlighted storage location `[fulltext] location`.
    fn make_default(&mut self) {
        let Some(picker) = &self.setup_picker else { return };
        let Some(option) = picker.options.get(picker.sel).filter(|o| !o.new) else { return };
        let name = option.name.clone();
        let result = match picker.kind {
            setup::Kind::Llm => self.lib.update_config(|t| {
                t.insert("default_llm".into(), name.clone().into());
                Ok(())
            }),
            setup::Kind::Storage => self.lib.update_config(|t| {
                config_section(t, "fulltext")?.insert("location".into(), name.clone().into());
                Ok(())
            }),
            _ => return,
        };
        self.after_config_change(result);
        self.refresh_setup_picker();
        self.status = format!("{name} is the default");
    }

    /// Saves a `:setup` form whose connection test passed: the secret to the secret store, the rest to yeda.toml.
    pub fn poll_setup(&mut self) {
        self.poll_models();
        let Some(job) = &self.setup_job else { return };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("the connection test stopped")),
        };
        let job = self.setup_job.take().unwrap();
        let Some(form) = &mut self.setup else { return };
        form.testing = false;
        form.sign_in_url = None;
        let secret = match result {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("not saved: {e:#}");
                return;
            }
        };
        let kind = form.kind;
        let mut stored = String::new();
        // An LLM profile form tests with its provider's credential and has no account of its own.
        if let Some(secret) = secret
            && !setup::account(&job.setup).is_empty()
        {
            let account = setup::account(&job.setup);
            if let Err(e) = yeda_secrets::set(&account, &secret) {
                stored = format!(" (secret kept for this session only: {e:#})");
            }
            self.secrets.insert(account.clone(), secret.clone());
            self.rejected.remove(&account);
            self.models_for = None;
        }
        if kind == setup::Kind::Openalex {
            self.close_setup();
            self.status = format!("saved the OpenAlex key to the secret store{stored}");
            return;
        }
        if let setup::Setup::ApiKey(provider) = &job.setup {
            let provider = *provider;
            crate::discover::clear_state(provider);
            self.setup = None;
            self.setup_from_web_search = false;
            if let Some(form) = &mut self.web_search {
                form.refresh();
            }
            self.mode = if self.web_search.is_some() { Mode::WebSearch } else { Mode::Normal };
            self.status = format!("saved the {} key to the secret store{stored}", provider.name());
            return;
        }
        if let setup::Setup::Searxng(url) = &job.setup {
            let url = url.clone();
            let warning = if url.contains('?') || url.contains('#') { "; the query string is stored in yeda.toml, which syncs through git; a token does not belong there" } else { "" };
            let applied = match self.lib.update_config(|t| {
                config_section(t, "web_search")?.insert("searxng_url".into(), url.into());
                Ok(())
            }) {
                Ok(effect) => self.apply(Ok(vec![effect])),
                Err(e) => {
                    self.reload();
                    self.status = format!("{e:#}");
                    false
                }
            };
            if !applied {
                return;
            }
            self.reload();
            crate::discover::clear_down(yeda_core::web_search::Provider::Searxng);
            self.setup = None;
            self.setup_from_web_search = false;
            if let Some(form) = &mut self.web_search {
                form.settings.searxng_url = self.lib.web_search.searxng_url.clone();
                form.refresh();
            }
            self.mode = if self.web_search.is_some() { Mode::WebSearch } else { Mode::Normal };
            self.status = format!("searxng saved; enable it here{warning}{stored}");
            return;
        }
        self.save_setup(job.section, &stored);
    }

    /// Enter on an edited provider whose key field is empty: the key stored for the old name and URL
    /// moves to the new ones, after a y/n when the URL changed, since the key is then sent elsewhere.
    fn keep_provider_key(&mut self, section: toml::Table, name: String, p: yeda_core::llm::Provider) {
        let Some(form) = &mut self.setup else { return };
        let Some((old_name, old_url)) = form.origin.clone() else { return };
        let (old, new) = (yeda_secrets::provider_account(&old_name, &old_url), yeda_secrets::provider_account(&name, p.url()));
        let alias = self.lib.providers.get(&old_name).and_then(|o| o.alias.clone());
        let stored = self.secrets.get(&old).cloned().or_else(|| yeda_secrets::stored(&old).ok().flatten());
        let from_env = yeda_secrets::from_env(&new, p.alias.as_deref()).is_some();
        let codex = p.auth == yeda_core::llm::Auth::Oauth && p.protocol == yeda_core::llm::Protocol::OpenaiResponses;
        if old != new && codex {
            return self.test_and_save_setup(section, setup::Setup::Provider(name, p));
        }
        if stored.is_none() && !from_env && yeda_secrets::from_env(&old, alias.as_deref()).is_none() {
            self.status = format!("no key is stored for '{old_name}'; type one in the last field");
            return;
        }
        if old == new || (stored.is_none() && from_env) {
            return self.save_setup(section, "");
        }
        let Some(secret) = stored else {
            self.status = "the old key comes from an environment variable; type the key for the new URL in the last field".into();
            return;
        };
        if old_url != p.url() && !std::mem::take(&mut form.moving_confirmed) {
            form.confirm_move = true;
            self.status = format!("move the key of '{old_name}' from {old_url} to {}? (y/n)", p.url());
            return;
        }
        form.moving_confirmed = false;
        let note = match yeda_secrets::set(&new, &secret) {
            Ok(()) => String::new(),
            Err(e) => format!(" (key kept for this session only: {e:#})"),
        };
        self.secrets.insert(new, secret);
        self.save_setup(section, &note);
    }

    /// Tests the connection the form describes in a thread, signing in first when a Codex provider has no credential; saved when it passes.
    fn test_and_save_setup(&mut self, section: toml::Table, setup: setup::Setup) {
        let Some(form) = &mut self.setup else { return };
        let secret = form.secret();
        let (tx, rx) = mpsc::channel();
        let account = setup::account(&setup);
        let stored_login = || yeda_secrets::get(&account).ok().flatten();
        let probe = setup.clone();
        let test_secret = match &setup {
            setup::Setup::Searxng(_) => secret.clone().or_else(stored_login),
            _ => secret.clone(),
        };
        let providers = self.lib.providers.clone();
        // An empty field asks a Codex provider to sign in instead: stored credentials
        // reauthorize with their own client and host id, a missing one registers anew.
        let codex_empty = secret.is_none()
            && matches!(&setup, setup::Setup::Provider(_, p) if p.auth == yeda_core::llm::Auth::Oauth && p.protocol == yeda_core::llm::Protocol::OpenaiResponses);
        if codex_empty {
            let (issued, host, hint) = yeda_secrets::stored(&account)
                .ok()
                .flatten()
                .and_then(|c| yeda_core::llm::CodexCredential::parse(&c).ok())
                .map_or((None, None, None), |c| (c.client_id, c.host_id, c.id_token));
            let Ok(sign_in) = yeda_net::codex::start(issued.as_deref(), host.as_deref(), hint.as_deref()) else {
                self.status = "could not start the sign-in listener".into();
                return;
            };
            let url = sign_in.url.clone();
            form.sign_in_url = Some(url.clone());
            std::thread::spawn(move || {
                let signed = sign_in.finish(std::time::Duration::from_secs(10 * 60));
                let _ = tx.send(match signed {
                    Ok(credentials) => {
                        // Stored before the test, so fixing the model does not ask for another sign-in.
                        let _ = yeda_secrets::set(&account, &credentials);
                        setup::test(&providers, &probe, Some(&credentials)).map(|_| Some(credentials))
                    }
                    Err(e) => Err(e),
                });
            });
            form.testing = true;
            self.status = "waiting for the browser to sign in with ChatGPT…".into();
            self.setup_job = Some(SetupJob { rx, section, setup });
            self.xdg_open(&url);
            return;
        }
        std::thread::spawn(move || {
            let _ = tx.send(setup::test(&providers, &probe, test_secret.as_deref()).map(|_| secret));
        });
        form.testing = true;
        self.status = "testing the connection…".into();
        self.setup_job = Some(SetupJob { rx, section, setup });
    }

    /// Enter on a Dropbox form: with no sign-in to use, shows the link and waits for the code
    /// pasted back; otherwise tests the sign-in typed or saved.
    fn submit_cloud(&mut self, section: toml::Table, name: String, remote: library::Remote) {
        let Some(form) = &mut self.setup else { return };
        let typed = form.secret();
        let again = typed.as_deref() == Some("signin");
        let typed = typed.filter(|_| !again);
        let pending = form.code_link.take();
        let account = yeda_secrets::remote_account(&name, &remote);
        let saved = if again { None } else { yeda_secrets::get(&account).ok().flatten() };
        let setup = setup::Setup::Cloud(name.clone(), remote.clone());
        let (tx, rx) = mpsc::channel();
        match (&remote, typed.or(saved)) {
            (library::Remote::Dropbox(cloud), Some(text)) if pending.is_some() => {
                let (verifier, cloud) = (pending.map(|p| p.1).unwrap_or_default(), cloud.clone());
                std::thread::spawn(move || {
                    let result = yeda_net::dropbox::finish(&cloud, &verifier, &text).and_then(|secret| {
                        // Stored before the test, so fixing the folder does not need another code.
                        let _ = yeda_secrets::set(&account, &secret);
                        yeda_net::remote::login(&remote, &secret).map(|()| Some(secret))
                    });
                    let _ = tx.send(result);
                });
            }
            (_, Some(secret)) => {
                std::thread::spawn(move || {
                    let _ = tx.send(yeda_net::remote::login(&remote, &secret).map(|()| Some(secret)));
                });
            }
            (library::Remote::Dropbox(cloud), None) => {
                match yeda_net::dropbox::start(cloud) {
                    Ok(sign_in) => {
                        form.code_link = Some((sign_in.url.clone(), sign_in.verifier));
                        self.status = "allow yeda in the browser, then paste the code Dropbox shows into Code".into();
                        self.xdg_open(&sign_in.url);
                    }
                    Err(e) => self.status = format!("{e:#}"),
                }
                return;
            }
            (library::Remote::Webdav(_), None) => return,
        }
        self.status = "testing the connection…".into();
        if let Some(form) = &mut self.setup {
            form.testing = true;
        }
        self.setup_job = Some(SetupJob { rx, section, setup });
    }

    /// Writes the form's section into yeda.toml, keeping the keys it has no field for.
    fn save_setup(&mut self, section: toml::Table, stored: &str) {
        let Some(form) = &self.setup else { return };
        let name = form.name().to_string();
        // The keys a form leaves out when their field is empty.
        let (path, omitted_when_empty): (Vec<&str>, &[&str]) = match form.kind {
            setup::Kind::Provider => (vec!["providers", &name], &["alias"]),
            setup::Kind::Llm => (vec!["llm", &name], &[]),
            setup::Kind::Table => (vec!["tables", &name], &["describe"]),
            setup::Kind::Proxy => (vec!["fulltext"], &["proxies"]),
            setup::Kind::Storage => (vec!["locations", &name], &["kind", "url", "user", "alias", "folder"]),
            _ => (vec!["locations", &name], &[]),
        };
        let renamed_from = form.origin.as_ref().map(|(old, _)| old.clone()).filter(|old| form.kind == setup::Kind::Provider && *old != name);
        let retired = form
            .origin
            .as_ref()
            .filter(|_| form.kind == setup::Kind::Provider)
            .and_then(|(old, url)| Some((yeda_secrets::provider_account(old, url), yeda_secrets::provider_account(&name, section.get("base_url")?.as_str()?))))
            .filter(|(old, new)| old != new)
            .map(|(old, _)| old);
        let result = self.lib.update_config(|t| {
            if let Some(old) = &renamed_from {
                rename_provider(t, old, &name);
            }
            let mut table = t;
            for key in &path {
                let entry = table.entry(*key).or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if !entry.is_table() {
                    *entry = toml::Value::Table(toml::Table::new());
                }
                table = entry.as_table_mut().unwrap();
            }
            for key in omitted_when_empty {
                table.remove(*key);
            }
            table.extend(section);
            Ok(())
        });
        let saved = result.is_ok();
        self.after_config_change(result);
        if let Some(old) = retired.filter(|_| saved) {
            let _ = yeda_secrets::delete(&old);
            self.secrets.remove(&old);
        }
        self.close_setup();
        self.status = format!("saved [{}] to yeda.toml{stored}", path.join("."));
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
    fn set_search(&mut self, query: &str) {
        self.pane = Pane::List;
        self.query = query.to_string();
        self.refilter();
        self.status = format!("{} references match", self.visible.len());
    }

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
        let pdfs = ops::pdfs_by_annotations(&self.dir, r);
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
        let hit = self.help_hit.take();
        if self.help_typing {
            match k.code {
                KeyCode::Esc => {
                    self.help_query.clear();
                    self.help_typing = false;
                }
                KeyCode::Enter => {
                    self.help_typing = false;
                    self.help_jump(true, None);
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
            KeyCode::Char('n') => self.help_jump(true, hit),
            KeyCode::Char('N') => self.help_jump(false, hit),
            KeyCode::Char('d') if ctrl => self.help_scroll = self.help_scroll.saturating_add(half),
            KeyCode::Char('u') if ctrl => self.help_scroll = self.help_scroll.saturating_sub(half),
            KeyCode::Char('j') | KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
            KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::Char('G') => self.help_scroll = u16::MAX,
            _ => {}
        }
    }

    /// Goes to the next (or previous) line matching `help_query` after the match `from`, else from the
    /// top line on, and scrolls it to the top.
    fn help_jump(&mut self, forward: bool, from: Option<usize>) {
        let query = self.help_query.to_lowercase();
        if query.is_empty() {
            return;
        }
        let hits: Vec<usize> = self.help_lines.iter().enumerate().filter(|(_, l)| l.to_lowercase().contains(&query)).map(|(i, _)| i).collect();
        // The scroll stops at the end of the page, so on the last screen it can't tell which match is current.
        let (after, before) = from.map_or((self.help_scroll as usize, self.help_scroll as usize), |i| (i + 1, i));
        let next = if forward {
            hits.iter().find(|&&i| i >= after).or(hits.first())
        } else {
            hits.iter().rfind(|&&i| i < before).or(hits.last())
        };
        match next {
            Some(&i) => {
                self.help_scroll = i as u16;
                self.help_hit = Some(i);
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
                        self.job = Some(Job::Fetch(Batch { step: self.step.clone(), collection, join_existing: false, mark_result: !self.plan_queue.is_empty(), total: todo.len(), todo, added: vec![], existed: vec![], failed: vec![] }));
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
                ("search", query) if !query.trim().is_empty() => self.set_search(query.trim()),
                (cmd @ ("summarize" | "summarize!"), args) => match options(args, &["llm"], &[]) {
                    Ok((_, rest)) if rest.is_empty() && self.lib.tables.len() > 1 => self.pick_table(cmd, args.trim()),
                    Ok((opts, rest)) => {
                        if let Err(e) = self.summarize(&rest, opts.get("llm").and_then(|v| v.last()).copied(), cmd.ends_with('!'), text) {
                            self.status = format!("{e:#}");
                        }
                    }
                    Err(e) => self.status = format!("{e:#}"),
                },
                ("help", _) => self.open_help(HelpPage::Guide),
                ("keys", _) => self.open_help(HelpPage::Keys),
                ("secrets", _) => self.open_secrets(),
                ("envvars", _) => self.open_envvars(),
                ("do", args) => {
                    if let Err(e) = self.assist(args, text) {
                        self.status = format!("{e:#}");
                    }
                }
                (cmd @ ("discover" | "discover!"), args) => {
                    if let Err(e) = self.discover(args, text, cmd.ends_with('!')) {
                        self.status = format!("{e:#}");
                    }
                }
                ("found", _) if self.found.is_empty() => self.status = "nothing found yet: :discover".into(),
                ("found", _) => self.mode = Mode::Found,
                ("history", _) => {
                    self.input.clear();
                    self.open_history();
                }
                ("setup", rest) if rest.trim().is_empty() => self.open_setup_menu(),
                ("setup", rest) => {
                    let kind = match rest.trim() {
                        "web-search" => return self.open_web_search(),
                        "provider" => setup::Kind::Provider,
                        "llm" => setup::Kind::Llm,
                        "storage" => setup::Kind::Storage,
                        "table" => setup::Kind::Table,
                        "openalex" => setup::Kind::Openalex,
                        "proxy" => setup::Kind::Proxy,
                        _ => {
                            self.status = "usage: :setup [provider|llm|storage|table|openalex|web-search|proxy]".into();
                            return;
                        }
                    };
                    self.setup_from_web_search = false;
                    if setup::listed(kind) {
                        self.open_setup_picker(kind);
                    } else {
                        let mut form = SetupForm::new(kind, "", &self.lib);
                        form.key_stored = kind == setup::Kind::Openalex && yeda_secrets::get(yeda_secrets::OPENALEX).ok().flatten().is_some();
                        self.setup = Some(form);
                        self.mode = Mode::Setup;
                    }
                }
                ("config", which) => match which.trim() {
                    "" | "library" => self.edit = Some((self.path(library::CONFIG), None)),
                    "device" => match dirs::config_dir() {
                        Some(d) => self.edit = Some((d.join("yeda/config.toml"), None)),
                        None => self.status = "no config folder on this device".into(),
                    },
                    _ => self.status = "usage: :config [library|device]".into(),
                },
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
                ("storage", args) => self.storage(args, text),
                ("detach", _) => self.detach(text),
                ("cache", _) => self.cache_pdfs(text),
                ("new", _) => {
                    self.setup = Some(SetupForm::new(setup::Kind::Reference, "", &self.lib));
                    self.mode = Mode::Setup;
                }
                ("sync", _) => self.start_git("sync", crate::sync::run),
                ("diff", _) => self.open_diff(),
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
                if let Some(provider) = crate::websearch::provider_of_account(&p.account) {
                    crate::discover::clear_state(provider);
                }
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

    /// `args` is `[table] [llm profile]`; `all` covers every visible reference instead of the selected one.
    /// `command` is the whole command line, run again once a missing key is entered.
    /// The API key or OAuth credentials stored for `b`'s provider, else a prompt naming it, after
    /// which `command` runs again. `None` while prompting.
    fn llm_key(&mut self, b: &yeda_core::llm::Bound, command: &str) -> Option<Option<String>> {
        let account = yeda_secrets::provider_account(&b.provider, b.url());
        let got = if b.needs_key() {
            let what = match (b.auth(), b.protocol()) {
                (yeda_core::llm::Auth::Oauth, yeda_core::llm::Protocol::OpenaiResponses) => "OAuth credentials (the compacted `~/.codex/auth.json`)",
                (yeda_core::llm::Auth::Oauth, _) => "OAuth token (from `claude setup-token`)",
                _ => "API key",
            };
            let prompt = format!("{what} for provider '{}', used by {}, sent to {}", b.provider, b.profile, b.url());
            self.secret(&account, &prompt, Retry::Command(command.into())).map(Some)
        } else {
            Some(yeda_secrets::get_as(&account, b.settings.alias.as_deref()).ok().flatten())
        };
        got.map(|key| self.renewed_codex(b, account, key))
    }

    /// Replaces Codex credentials that are about to expire; a failed renewal keeps the old ones,
    /// whose expiry the request reports.
    fn renewed_codex(&mut self, b: &yeda_core::llm::Bound, account: String, key: Option<String>) -> Option<String> {
        if b.protocol() != yeda_core::llm::Protocol::OpenaiResponses {
            return key;
        }
        if yeda_secrets::from_env(&account, b.settings.alias.as_deref()).is_some() || !key.as_deref().is_some_and(crate::setup::codex_refresh_due) {
            return key;
        }
        let key = key.unwrap();
        match yeda_net::llm::codex_refresh(&key) {
            Ok(renewed) => {
                if let Err(e) = yeda_secrets::set(&account, &renewed) {
                    self.status = format!("kept for this session only, not saved: {e:#}");
                }
                self.secrets.insert(account.clone(), renewed.clone());
                self.rejected.remove(&account);
                Some(renewed)
            }
            Err(e) => {
                self.status = format!("{e:#}; ");
                Some(key)
            }
        }
    }

    /// `:discover`: runs with the papers and description it names, or opens the form when it names none.
    fn discover(&mut self, args: &str, command: &str, fresh: bool) -> anyhow::Result<()> {
        let (opts, prompt) = options(args, &["llm"], &["like"])?;
        let llm = opts.get("llm").and_then(|v| v.last()).copied();
        let like = opts.contains_key("like");
        if prompt.trim().is_empty() && !like {
            self.open_discover(llm);
            return Ok(());
        }
        let from: Vec<Ref> = if like {
            anyhow::ensure!(!self.marked.is_empty(), "--like needs marked references (v, V)");
            self.lib.refs.iter().filter(|r| self.marked.contains(&r.key)).cloned().collect()
        } else {
            Vec::new()
        };
        self.start_discover(llm, prompt, from, command, fresh)
    }

    fn open_discover(&mut self, llm: Option<&str>) {
        let marked: Vec<&Ref> = self.lib.refs.iter().filter(|r| self.marked.contains(&r.key)).collect();
        self.discover_form = Some(crate::discoverform::DiscoverForm::new(&self.lib, &marked, llm));
        self.mode = Mode::Discover;
    }

    fn discover_form_key(&mut self, k: KeyEvent) {
        use crate::discoverform::Outcome;
        let Some(form) = &mut self.discover_form else {
            self.mode = Mode::Normal;
            return;
        };
        let fresh = match form.key(k, &self.lib.refs) {
            Outcome::Stay => return,
            Outcome::Close => {
                self.discover_form = None;
                self.mode = Mode::Normal;
                return;
            }
            Outcome::Search { fresh } => fresh,
        };
        let from: Vec<Ref> = form.papers.iter().filter_map(|p| self.lib.get(&p.key).cloned()).collect();
        let (prompt, llm) = (form.text.trim().to_string(), form.llm_name().map(String::from));
        if from.is_empty() && prompt.is_empty() {
            return self.status = "add a paper or write what to look for first".into();
        }
        let command = if fresh { "discover!" } else { "discover" };
        match self.start_discover(llm.as_deref(), prompt, from, command, fresh) {
            Ok(()) => {
                self.discover_form = None;
                self.mode = Mode::Normal;
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// Asks the LLM profile `llm`, else the default one, for papers from what is given.
    fn start_discover(&mut self, llm: Option<&str>, prompt: String, from: Vec<Ref>, command: &str, fresh: bool) -> anyhow::Result<()> {
        if self.discover_job.is_some() {
            anyhow::bail!("already searching");
        }
        let (name, profile) = self.lib.llm_profile(llm)?;
        let Some(key) = self.llm_key(&profile, command) else { return Ok(()) };
        let openalex = self.openalex_key();
        let settings = self.lib.web_search.clone();
        let mut keys = HashMap::new();
        for (provider, account) in [
            (yeda_core::web_search::Provider::Keenable, yeda_secrets::KEENABLE),
            (yeda_core::web_search::Provider::Exa, yeda_secrets::EXA),
            (yeda_core::web_search::Provider::Parallel, yeda_secrets::PARALLEL),
            (yeda_core::web_search::Provider::Firecrawl, yeda_secrets::FIRECRAWL),
        ] {
            if let Ok(Some(k)) = yeda_secrets::get(account) {
                keys.insert(provider, k);
            }
        }
        keys.extend(crate::websearch::searxng_login(&settings));
        // The server is named, since a shared yeda.toml decides where the text goes.
        let server = profile.url().to_string();
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let mut used = Vec::new();
        if !from.is_empty() {
            used.push(plural(from.len(), "paper"));
        }
        if !prompt.trim().is_empty() {
            used.push("a description".into());
        }
        self.status = format!("asking {name} at {server} with {}…", used.join(", "));
        self.discover_job = Some(crate::discover::spawn(crate::discover::Input { profile, key, prompt, from, openalex_key: openalex, settings, keys, cache: self.lib.cache.clone(), fresh }));
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

    fn open_diff(&mut self) {
        match crate::sync::changes(&self.dir) {
            Ok(changes) if changes.is_empty() => self.status = "no changes since the last sync".into(),
            Ok(changes) => {
                self.diff = Some(DiffView { changes, sel: 0, marked: BTreeSet::new(), text: String::new(), scroll: 0, d: false, confirming: false });
                self.show_diff(0);
                self.mode = Mode::Diff;
            }
            Err(e) => self.status = format!("diff: {e:#}"),
        }
    }

    fn show_diff(&mut self, sel: usize) {
        let Some(v) = &mut self.diff else { return };
        v.sel = sel.min(v.changes.len().saturating_sub(1));
        v.scroll = 0;
        v.text = match v.changes.get(v.sel) {
            Some(c) => crate::sync::diff(&self.dir, c).unwrap_or_else(|e| format!("{e:#}")),
            None => String::new(),
        };
    }

    fn diff_key(&mut self, k: KeyEvent) {
        let Some(v) = &mut self.diff else { return self.mode = Mode::Normal };
        if std::mem::take(&mut v.confirming) {
            match k.code {
                KeyCode::Char('y') => self.undo_changes(),
                _ => self.status = "cancelled".into(),
            }
            return;
        }
        let (sel, d, ctrl) = (v.sel, std::mem::take(&mut v.d), k.modifiers.contains(KeyModifiers::CONTROL));
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.show_diff(sel + 1),
            KeyCode::Char('k') | KeyCode::Up => self.show_diff(sel.saturating_sub(1)),
            KeyCode::Char('d') if ctrl => v.scroll = v.scroll.saturating_add(10),
            KeyCode::Char('u') if ctrl => v.scroll = v.scroll.saturating_sub(10),
            KeyCode::Char('v') => {
                if !v.marked.remove(&sel) {
                    v.marked.insert(sel);
                }
                self.show_diff(sel + 1);
            }
            KeyCode::Char('d') if d => {
                v.confirming = true;
                self.status = match v.marked.len() {
                    0 => format!("undo the changes to {}? (y/n)", v.changes[sel].path),
                    1 => format!("undo the changes to {}? (y/n)", v.changes[*v.marked.first().unwrap()].path),
                    n => format!("undo the changes to {n} marked files? (y/n)"),
                };
            }
            KeyCode::Char('d') => v.d = true,
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn undo_changes(&mut self) {
        let Some(v) = &self.diff else { return };
        let picked: Vec<&crate::sync::Change> = match v.marked.is_empty() {
            true => v.changes.get(v.sel).into_iter().collect(),
            false => v.marked.iter().filter_map(|&i| v.changes.get(i)).collect(),
        };
        let n = picked.len();
        let (result, sel) = (crate::sync::revert(&self.dir, &picked), v.sel);
        self.reload();
        self.mode = Mode::Normal;
        self.open_diff();
        self.show_diff(sel);
        self.status = match result {
            Ok(()) => format!("undid the changes to {n} file{}", if n == 1 { "" } else { "s" }),
            Err(e) => format!("{e:#}"),
        };
    }

    /// `:do`: asks the LLM for a plan for `args`; the plan is shown for approval when it arrives.
    fn assist(&mut self, args: &str, command: &str) -> anyhow::Result<()> {
        let (opts, request) = options(args, &["llm"], &[])?;
        anyhow::ensure!(!request.trim().is_empty(), "usage: :do [--llm <profile>] <request>");
        anyhow::ensure!(self.assist_job.is_none() && self.plan.is_none() && self.plan_queue.is_empty(), "a plan is already pending or running");
        let (name, profile) = self.lib.llm_profile(opts.get("llm").and_then(|v| v.last()).copied())?;
        let Some(key) = self.llm_key(&profile, command) else { return Ok(()) };
        let marked = !self.marked.is_empty();
        let targets: Vec<&Ref> = self.targets().into_iter().map(|i| &self.lib.refs[i]).collect();
        let usage: Vec<(String, &str)> = COMMANDS.iter().map(|(u, m)| (u.to_string(), *m)).chain(ACTIONS.iter().map(|(a, m)| (a.to_string(), *m))).collect();
        let usage: Vec<(&str, &str)> = usage.iter().map(|(u, m)| (u.as_str(), *m)).collect();
        let text = yeda_core::assist::request(&usage, &self.lib, &targets, marked, &request);
        self.assist_request = request.to_string();
        self.status = format!("asking {name} at {} for a plan…", profile.url());
        self.assist_job = Some(crate::assist::spawn(profile, key, text, yeda_core::assist::schema(&self.lib)));
        Ok(())
    }

    pub fn poll_assist(&mut self) {
        let Some(rx) = &self.assist_job else { return };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the request stopped".into()),
        };
        self.assist_job = None;
        let raw = answer.clone().unwrap_or_default();
        match answer.map_err(anyhow::Error::msg).and_then(|a| yeda_core::assist::parse_plan(&a, &self.lib, &self.assist_request)) {
            Ok(plan) if plan.steps.is_empty() => {
                self.log.write(&format!("do: no plan: {}", plan.reply));
                self.status = plan.reply;
            }
            Ok(plan) => {
                for (i, step) in plan.steps.iter().enumerate() {
                    self.log.write(&format!("do: plan {}/{}: {} — {}", i + 1, plan.steps.len(), step.command(), step.reason));
                }
                self.plan = Some(plan);
                self.mode = Mode::Plan;
            }
            Err(e) => {
                let answer: String = raw.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(500).collect();
                self.log.write(&format!("do: failed: {e:#}; answer: {answer}"));
                self.status = format!("do: {e:#}");
            }
        }
    }

    fn plan_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Enter => {
                self.plan_queue = self.plan.take().map(|p| p.steps.iter().map(yeda_core::assist::Step::command).collect()).unwrap_or_default();
                self.plan_total = self.plan_queue.len();
                self.plan_running = true;
                self.log.write("do: approved");
                self.mode = Mode::Normal;
                self.status = format!("running {} steps…", self.plan_queue.len());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.plan = None;
                self.log.write("do: cancelled");
                self.mode = Mode::Normal;
                self.status = "cancelled".into();
            }
            _ => {}
        }
    }

    /// Runs the approved plan's next command once the one before has finished; called from the event loop.
    pub fn poll_plan(&mut self) {
        if self.mode != Mode::Normal || self.job.is_some() || !self.busy().is_empty() {
            return;
        }
        let Some(command) = self.plan_queue.pop_front() else {
            if std::mem::take(&mut self.plan_running) {
                self.log.write("do: done");
            }
            return;
        };
        self.log.write(&format!("do: step {}/{}: {command}", self.plan_total - self.plan_queue.len(), self.plan_total));
        self.confirmed = true;
        self.submit(Prompt::Command, &command);
        self.confirmed = false;
    }

    pub fn poll_discover(&mut self) {
        let Some(rx) = &self.discover_job else { return };
        loop {
            match rx.try_recv() {
                Ok(crate::discover::Msg::Progress(line)) => self.status = line,
                Ok(crate::discover::Msg::Done(result)) => {
                    self.discover_job = None;
                    match result {
                        Ok((found, note)) => {
                            self.status = discover_status(&found, note.as_deref());
                            if found.is_empty() {
                                return;
                            }
                            self.found_note = note;
                            self.found = found;
                            let mut found = std::mem::take(&mut self.found);
                            found.sort_by_key(|c| self.found_in_library(c));
                            self.found = found;
                            self.found_sel = 0;
                            self.found_marked.clear();
                            self.mode = Mode::Found;
                        }
                        Err(e) => self.status = format!("discover: {e:#}"),
                    }
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.discover_job = None;
                    self.status = "discover: the search stopped".into();
                    return;
                }
            }
        }
    }

    /// Whether the suggestion is in the library already, by DOI or title.
    pub fn found_in_library(&self, c: &Candidate) -> bool {
        c.in_library(&self.lib).is_some()
    }

    fn found_key(&mut self, k: KeyEvent) {
        let n = self.found.len();
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.found_sel = (self.found_sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => self.found_sel = self.found_sel.saturating_sub(1),
            KeyCode::Char('v') => {
                if !self.found_marked.remove(&self.found_sel) {
                    self.found_marked.insert(self.found_sel);
                }
                self.found_sel = (self.found_sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Char('a') | KeyCode::Enter if self.job.is_some() => self.status = "still adding; try again in a moment".into(),
            KeyCode::Char('a') | KeyCode::Enter => {
                let picked: Vec<&Candidate> = match self.found_marked.is_empty() {
                    true => self.found.get(self.found_sel).into_iter().collect(),
                    false => self.found_marked.iter().filter_map(|&i| self.found.get(i)).collect(),
                };
                let ids: Vec<String> = picked.iter().filter(|c| !self.found_in_library(c)).filter_map(|c| c.id()).collect();
                match (picked.as_slice(), ids.is_empty()) {
                    ([], _) => {}
                    ([c], true) if self.found_in_library(c) => self.status = "already in the library".into(),
                    ([_], true) => self.status = "no DOI or URL to add it by".into(),
                    (_, true) => self.status = "the marked papers are in the library or have no DOI or URL".into(),
                    (_, false) => self.submit(Prompt::Add, &ids.join("\n")),
                }
            }
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

    fn summarize(&mut self, args: &str, llm: Option<&str>, overwrite: bool, command: &str) -> anyhow::Result<()> {
        if self.summarizer.is_some() {
            anyhow::bail!("a summarization is already running (:cancel stops it)");
        }
        let mut words = args.split_whitespace();
        let table_name = ops::table_name(&self.lib, words.next())?;
        if words.next().is_some() {
            anyhow::bail!("usage: :summarize[!] [table] [--llm <profile>]");
        }
        let (profile_name, profile) = self.lib.llm_profile(llm)?;
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
        // The server is named, since a shared yeda.toml decides where the paper goes.
        let server = profile.url().to_string();
        std::thread::spawn(move || {
            for (i, job) in jobs.into_iter().enumerate() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(WorkerMsg::Started(format!("summarizing {} ({}/{total}) with {profile_name} at {server}…", job.key, i + 1)));
                let paper = job.key.clone();
                let result = ops::summarize(&profile, job, key.as_deref()).map_err(|e| format!("{e:#}"));
                let _ = tx.send(WorkerMsg::Finished { key: paper, result });
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

    /// The background jobs still running, by what they do.
    pub fn busy(&self) -> Vec<&'static str> {
        [
            (self.discover_job.is_some(), "discover"),
            (self.assist_job.is_some(), "plan"),
            (self.summarizer.is_some(), "summarize"),
            (self.fetch_job.is_some(), "fetch"),
            (self.setup_job.is_some(), "connection test"),
            (self.model_job.is_some(), "model list"),
            (self.graph_job.is_some(), "graph"),
            (self.text_job.is_some(), "text extraction"),
            (self.cache_job.is_some(), "cache"),
            (self.migrate_job.is_some(), "migration"),
        ]
        .into_iter()
        .filter_map(|(running, what)| running.then_some(what))
        .collect()
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
        let path = ops::expand_home(path.trim());
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
        let alias = self.alias_of(account);
        if let Some(s) = yeda_secrets::from_env(account, alias.as_deref()) {
            if self.rejected.contains(account) {
                self.status = Self::rejected_hint(account, alias.as_deref(), "");
                return None;
            }
            return Some(s);
        }
        if let Some(s) = self.secrets.get(account) {
            return Some(s.clone());
        }
        if !self.rejected.contains(account) {
            match yeda_secrets::stored(account) {
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

    /// The alias of the provider or WebDAV location whose secret is `account`.
    fn alias_of(&self, account: &str) -> Option<String> {
        let provider = self.lib.providers.iter().find(|(n, p)| yeda_secrets::provider_account(n, p.url()) == account).and_then(|(_, p)| p.alias.clone());
        provider.or_else(|| self.lib.remotes.iter().find(|(n, r)| yeda_secrets::remote_account(n, r) == account).and_then(|(_, r)| r.alias().map(String::from)))
    }

    /// The secret of the location `name`: a WebDAV password, asked for when missing, or a cloud
    /// drive's sign-in, which only `:setup storage` makes.
    fn remote_secret(&mut self, name: &str, retry: Retry) -> Option<String> {
        let remote = self.lib.remotes.get(name)?.clone();
        let account = yeda_secrets::remote_account(name, &remote);
        if matches!(remote, library::Remote::Webdav(_)) {
            return self.secret(&account, &remote.label(), retry);
        }
        if let Some(s) = yeda_secrets::from_env(&account, None).or_else(|| self.secrets.get(&account).cloned()) {
            return Some(s);
        }
        match yeda_secrets::stored(&account) {
            Ok(Some(s)) if !self.rejected.contains(&account) => Some(s),
            Ok(_) => {
                self.status = format!("{name} is not signed in to {}; run :setup storage", remote.label());
                None
            }
            Err(e) => {
                self.status = format!("{e:#}");
                None
            }
        }
    }

    /// What to tell the user when the server turned down the secret of `account`. An environment
    /// variable cannot be replaced from inside yeda, so it is named instead of asked for again.
    fn rejected_hint(account: &str, alias: Option<&str>, command: &str) -> String {
        match yeda_secrets::env_hit(account, alias) {
            Some((name, _)) => format!("${name} was rejected by the server; fix or unset it and restart yeda"),
            None => format!("run {command} again to enter it"),
        }
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

    /// Every secret the configuration can use, as (account, label).
    fn secret_accounts(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = Vec::new();
        for (name, remote) in &self.lib.remotes {
            let what = if matches!(remote, library::Remote::Webdav(_)) { "password" } else { "sign-in token" };
            items.push((yeda_secrets::remote_account(name, remote), format!("Storage {name}: {what} for {}", remote.label())));
        }
        for (name, p) in &self.lib.providers {
            let what = match (p.auth, p.protocol) {
                (yeda_core::llm::Auth::None, _) => "no credential",
                (yeda_core::llm::Auth::Oauth, yeda_core::llm::Protocol::OpenaiResponses) => "OAuth credentials (the compacted `~/.codex/auth.json`)",
                (yeda_core::llm::Auth::Oauth, _) => "OAuth token",
                _ => "API key",
            };
            let profiles: Vec<&str> = self.lib.llm.iter().filter(|(_, q)| q.provider == *name).map(|(n, _)| n.as_str()).collect();
            let for_profiles = if profiles.is_empty() { String::new() } else { format!(", shared by {}", profiles.join(", ")) };
            items.push((yeda_secrets::provider_account(name, p.url()), format!("Provider {name}: {what} for {}{for_profiles}", p.url())));
        }
        if let Some(url) = &self.lib.web_search.searxng_url {
            let host = yeda_core::web_search::searxng_host(url).unwrap_or_default();
            items.push((yeda_secrets::searxng_account(url), format!("SearXNG {host}: user:password, optional (basic auth)")));
        }
        items.push((OPENALEX_ACCOUNT.into(), "OpenAlex API key, optional (free at openalex.org/settings/api)".into()));
        for (account, label) in [
            (yeda_secrets::KEENABLE, "Keenable API key, optional: keyless search uses the shared public tier"),
            (yeda_secrets::EXA, "Exa API key, optional: keyless search uses the shared free tier, direct IPv4 only"),
            (yeda_secrets::PARALLEL, "Parallel API key, optional: keyless search uses the shared free tier"),
            (yeda_secrets::FIRECRAWL, "Firecrawl API key, optional: keyless search uses the shared free tier"),
        ] {
            items.push((account.into(), label.into()));
        }
        let mut seen = HashSet::new();
        items.retain(|(account, _)| seen.insert(account.clone()));
        items
    }

    /// Every secret the configuration can use, with where each is set.
    fn open_secrets(&mut self) {
        let items = self.secret_accounts();
        self.secret_items = items
            .into_iter()
            .map(|(account, label)| {
                let alias = self.alias_of(&account);
                let status = match yeda_secrets::stored(&account) {
                    _ if yeda_secrets::from_env(&account, alias.as_deref()).is_some() => "envvar".into(),
                    _ if self.secrets.contains_key(&account) => "session".into(),
                    Ok(Some(_)) => "secret service".into(),
                    Ok(None) => "not set".into(),
                    Err(e) => {
                        self.status = format!("{e:#}");
                        "secret service unavailable".into()
                    }
                };
                SecretItem { account, label, status }
            })
            .collect();
        self.secret_sel = self.secret_sel.min(self.secret_items.len() - 1);
        self.mode = Mode::Secrets;
    }

    /// Every secret with the environment variables that can set it, and a note where it has to
    /// be given an alias first.
    fn open_envvars(&mut self) {
        let accounts = self.secret_accounts();
        let aliases: Vec<Option<String>> = accounts.iter().map(|(account, _)| self.alias_of(account)).collect();
        let mut sharing: HashMap<String, usize> = HashMap::new();
        for ((account, _), alias) in accounts.iter().zip(&aliases) {
            if let (None, Some(base)) = (alias, yeda_secrets::env_names(account, None).pop()) {
                *sharing.entry(base).or_default() += 1;
            }
        }
        self.secret_items = accounts
            .into_iter()
            .zip(aliases)
            .map(|((account, label), alias)| {
                let names = yeda_secrets::env_names(&account, alias.as_deref());
                let mut lines: Vec<String> = names.iter().map(|n| format!("${n}")).collect();
                if alias.is_none() {
                    let shared = names.last().and_then(|n| sharing.get(n)).copied().unwrap_or(1);
                    if names.is_empty() && !yeda_secrets::env_names(&account, Some("a")).is_empty() {
                        lines.push("needs an alias: without one its name could be another server's".into());
                    } else if names.is_empty() {
                        lines.push("no variable for this server".into());
                    } else if shared > 1 {
                        lines.push(format!("needs an alias: {shared} secrets on this server share this variable"));
                    }
                }
                SecretItem { account, label, status: lines.join("\n") }
            })
            .collect();
        self.secret_sel = self.secret_sel.min(self.secret_items.len().saturating_sub(1));
        self.mode = Mode::Envvars;
    }

    /// The variable `:envvars` highlights: the first name listed for the selected secret.
    fn selected_envvar(&self) -> Option<String> {
        let item = self.secret_items.get(self.secret_sel)?;
        item.status.lines().find_map(|l| l.strip_prefix('$')).map(String::from)
    }

    fn envvars_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Enter => {
                self.status = match self.selected_envvar() {
                    Some(name) => match self.copy(&name) {
                        Ok(()) => format!("copied {name} to the clipboard"),
                        Err(e) => format!("clipboard: {e}"),
                    },
                    None => "no variable for this secret".into(),
                };
            }
            KeyCode::Char('j') | KeyCode::Down => self.secret_sel = (self.secret_sel + 1).min(self.secret_items.len().saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => self.secret_sel = self.secret_sel.saturating_sub(1),
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
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
                let status = match yeda_secrets::delete(&account) {
                    Ok(()) => "removed".into(),
                    Err(e) => format!("{e:#}"),
                };
                if let Some(provider) = crate::websearch::provider_of_account(&account) {
                    crate::discover::clear_state(provider);
                }
                self.status = status;
                let status = std::mem::take(&mut self.status);
                self.open_secrets();
                self.status = status;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Downloads the selected reference's remote PDF into the cache, then opens it.
    fn download(&mut self, pdf: &Pdf, password: &str) {
        let (Some(remote), Ok(path)) = (self.lib.remote_pdf(pdf), self.lib.pdf_path(pdf)) else { return };
        let account = yeda_secrets::remote_account(remote.location, remote.remote);
        match ops::sync_pdf(remote.remote, password, &remote.path, &path) {
            Ok(()) => self.launch_reader(&path, pdf),
            Err(e) if yeda_net::remote::is_unauthorized(&e) => {
                self.reject(&account);
                self.open_pdf();
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    /// `:cache`: brings the cached copies of the targets' remote PDFs up to date in the background.
    fn cache_pdfs(&mut self, command: &str) {
        if self.cache_job.is_some() {
            return self.status = "already caching PDFs".into();
        }
        let pdfs: Vec<(String, library::Remote, String, PathBuf)> = self
            .targets()
            .iter()
            .flat_map(|&i| &self.lib.refs[i].pdfs)
            .filter_map(|p| Some((self.lib.remote_pdf(p)?, self.lib.pdf_path(p).ok()?)))
            .map(|(remote, path)| (remote.location.to_string(), remote.remote.clone(), remote.path, path))
            .collect();
        if pdfs.is_empty() {
            return self.status = "no remote PDFs".into();
        }
        let mut passwords = HashMap::new();
        for (name, remote, ..) in &pdfs {
            let account = yeda_secrets::remote_account(name, remote);
            if let std::collections::hash_map::Entry::Vacant(e) = passwords.entry(account) {
                let Some(p) = self.remote_secret(name, Retry::Command(command.into())) else { return };
                e.insert(p);
            }
        }
        self.status = format!("caching {} PDFs…", pdfs.len());
        let total = pdfs.len();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (name, remote, path, cached) in pdfs {
                let account = yeda_secrets::remote_account(&name, &remote);
                let result = ops::sync_pdf(&remote, &passwords[&account], &path, &cached);
                let rejected = result.as_ref().is_err_and(yeda_net::remote::is_unauthorized);
                if tx.send((account, path, result)).is_err() || rejected {
                    break;
                }
            }
        });
        self.cache_job = Some(CacheJob { rx, total, done: 0, failed: 0, error: None, rejected: false });
    }

    pub fn poll_cache(&mut self) {
        let Some(job) = &mut self.cache_job else { return };
        let before = job.done;
        let mut rejected = None;
        let finished = loop {
            match job.rx.try_recv() {
                Ok((account, remote, result)) => {
                    job.done += 1;
                    if let Err(e) = result {
                        job.failed += 1;
                        if yeda_net::remote::is_unauthorized(&e) {
                            job.rejected = true;
                            rejected = Some(account);
                        }
                        job.error = job.error.take().or(Some(format!("{remote}: {e:#}")));
                    }
                }
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        if job.done == before && !finished {
            return;
        }
        self.status = match (finished, &job.error) {
            (false, _) => format!("caching PDFs: {}/{}…", job.done, job.total),
            (true, None) => format!("{} PDFs cached", job.total),
            (true, Some(_)) if job.rejected => format!("the remote rejected the credentials; {}", Self::rejected_hint(rejected.as_deref().unwrap_or_default(), self.alias_of(rejected.as_deref().unwrap_or_default()).as_deref(), ":cache")),
            (true, Some(e)) => format!("{} of {} PDFs cached; {} failed: {e}", job.total - job.failed, job.total, job.failed),
        };
        if let Some(account) = rejected {
            self.reject(&account);
        }
        if finished {
            self.cache_job = None;
            if self.text_job.is_none() && self.searches_text() {
                self.index_text();
            }
        }
    }

    /// `:storage migrate <location>`: copies every PDF of the default location there, then makes
    /// it the default. The originals stay.
    fn storage(&mut self, args: &str, command: &str) {
        let (sub, to) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let to = to.trim();
        if sub != "migrate" || to.is_empty() {
            return self.status = "usage: :storage migrate <location>".into();
        }
        if self.migrate_job.is_some() {
            return self.status = "a migration is already running".into();
        }
        if !self.lib.locations.contains_key(to) && !self.lib.remotes.contains_key(to) {
            return self.status = format!("no location named '{to}'; :setup storage lists them");
        }
        let names: Vec<&String> = self.lib.locations.keys().chain(self.lib.remotes.keys()).collect();
        let from = match (&self.lib.fulltext.location, names.as_slice()) {
            (Some(l), _) => l.clone(),
            (None, [only]) => only.to_string(),
            (None, _) => {
                return self.status = format!("name the default location in [fulltext] location of yeda.toml ({})", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
            }
        };
        if from == to {
            return self.status = format!("{to} is the default location already");
        }
        let refs = self.lib.refs.iter().filter(|r| r.pdfs.iter().any(|p| p.location == from)).count();
        if refs == 0 {
            return self.status = format!("no PDFs in {from}");
        }
        if !self.confirm_many(&format!("move the PDFs of {from} to {to} for"), refs, command) {
            return;
        }
        let from_secret = if self.lib.remotes.contains_key(&from) {
            match self.remote_secret(&from, Retry::Command(command.into())) {
                Some(s) => Some(s),
                None => return,
            }
        } else {
            None
        };
        let to_secret = if self.lib.remotes.contains_key(to) {
            match self.remote_secret(to, Retry::Command(command.into())) {
                Some(s) => Some(s),
                None => return,
            }
        } else {
            None
        };
        let lib = self.lib.clone();
        let (from_name, to_name) = (from.clone(), to.to_string());
        let (tx, rx) = mpsc::channel();
        self.status = format!("moving {refs} of the PDFs in {from} to {to}…");
        std::thread::spawn(move || {
            let result = ops::migrate(&lib, &from_name, &to_name, from_secret.as_deref(), to_secret.as_deref())
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        self.migrate_job = Some(MigrateJob { rx, from, to: to.to_string() });
    }

    /// Applies a finished `:storage migrate`; called from the event loop.
    pub fn poll_migrate(&mut self) {
        let Some(job) = &self.migrate_job else { return };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the migration stopped".into()),
        };
        let job = self.migrate_job.take().unwrap();
        let (refs, moved) = match result {
            Ok(r) => r,
            Err(e) => return self.status = format!("not moved: {e}"),
        };
        let effects: Vec<Effect> = refs.iter().filter_map(|r| self.lib.save(r).ok()).collect();
        let config = self.lib.update_config(|t| {
            config_section(t, "fulltext")?.insert("location".into(), job.to.clone().into());
            Ok(())
        });
        let effects = match config {
            Ok(effect) => {
                let mut all = effects;
                all.push(effect);
                Ok(all)
            }
            Err(e) => Err(e),
        };
        if self.apply(effects) {
            self.reload();
            self.status = format!("moved {moved} PDFs to {}; the originals stay in {}", job.to, job.from);
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
        let password = if self.lib.remotes.contains_key(&location) {
            match self.remote_secret(&location, Retry::Command(command.into())) {
                Some(p) => Some(p),
                None => return,
            }
        } else {
            None
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

    fn attach_file(&self, i: usize, file: &std::path::Path, location: &str, password: Option<&str>) -> anyhow::Result<Ref> {
        ops::attach_file(&self.lib, &self.lib.refs[i], file, location, password)
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
            Err(e) if yeda_net::remote::is_unauthorized(&e) => {
                let account = self.lib.remotes.get(location).map(|r| yeda_secrets::remote_account(location, r));
                let hint = account.as_deref().map_or_else(String::new, |a| Self::rejected_hint(a, self.alias_of(a).as_deref(), ":attach"));
                if let Some(account) = account {
                    self.reject(&account);
                }
                self.status = format!("{e:#}; {hint}");
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
        ops::extract_annotations(&self.lib, &self.dir, &self.lib.refs[i], &mut |effects| self.write(&self.step, effects))
    }

    fn load_annotations(&self, key: &str, pdf: &Pdf) -> Vec<Annotation> {
        ops::load_annotations(&self.dir, key, pdf)
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

    /// `:add --to <collection> ids`: adds without the collection picker.
    fn add_to(&mut self, collection: &str, ids_text: &str) {
        let todo: std::collections::VecDeque<String> = ids::split_ids(ids_text).into_iter().map(Into::into).collect();
        if todo.is_empty() || collection.trim().is_empty() {
            self.status = "usage: :add --to <collection> <DOI, arXiv ID or ISBN …>".into();
            return;
        }
        self.status = format!("fetching 1/{}: {}…", todo.len(), todo[0]);
        let batch = Batch {
            step: self.step.clone(),
            collection: Some(collection.trim().to_string()),
            join_existing: true,
            mark_result: !self.plan_queue.is_empty(),
            total: todo.len(),
            todo,
            added: vec![],
            existed: vec![],
            failed: vec![],
        };
        self.job = Some(Job::Fetch(batch));
    }

    /// Fetches and saves one identifier; returns its key and whether it was already in the library.
    fn add_one(&mut self, input: &str, collection: Option<&String>, join_existing: bool, step: &undo::Step) -> anyhow::Result<(String, bool)> {
        let mut r = wand::fetch(input)?;
        if let Some(x) = r.doi.as_deref().and_then(|d| self.lib.find_doi(d)) {
            let mut x = x.clone();
            if let Some(c) = collection.filter(|c| join_existing && !x.collections.contains(c)) {
                x.collections.push(c.clone());
                self.write(step, vec![self.lib.save(&x)?])?;
                self.reload();
            }
            return Ok((x.key, true));
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
        match self.add_one(&input, batch.collection.as_ref(), batch.join_existing, &batch.step) {
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
        if batch.mark_result {
            self.marked = batch.added.iter().chain(&batch.existed).cloned().collect();
            if self.marked.is_empty() {
                let skipped = std::mem::take(&mut self.plan_queue);
                if std::mem::take(&mut self.plan_running) {
                    self.log.write(&format!("do: stopped: add found nothing; skipped {}", skipped.into_iter().collect::<Vec<_>>().join(", ")));
                }
            }
        }
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
        let name = match ops::table_name(&self.lib, Some(args)) {
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
        if path.exists() {
            return self.launch_reader(&path, &pdf);
        }
        let Some(remote) = self.lib.remote_pdf(&pdf) else { return self.status = format!("missing: {}", path.display()) };
        let (location, file) = (remote.location.to_string(), remote.path.clone());
        if let Some(password) = self.remote_secret(&location, Retry::OpenPdf) {
            self.status = format!("downloading {file}…");
            self.job = Some(Job::Download(pdf.clone(), password));
        }
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
        let effects = self.targets().iter().map(|&i| self.lib.delete(&self.lib.refs[i].key)).collect::<anyhow::Result<Vec<_>>>();
        self.apply(effects.map(|e| e.concat()));
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
        let unknown = match f.unknown.len() {
            0 => String::new(),
            n => format!(" · {n} not in OpenAlex (no DOI, or an unknown one)"),
        };
        let empty = f.graph.nodes.is_empty();
        let (mut graph, effects) = ops::finish_graph(&mut self.lib, f);
        self.apply(effects.into_iter().collect());
        if empty {
            self.status = format!("graph: nothing to show{unknown}");
            return;
        }
        let (tab, by_year, ego) = self.graph.as_ref().map_or((GraphTab::Cites, false, false), |g| (g.tab, g.by_year, g.ego));
        // A rebuild after opening a paper keeps the exploration; a new :graph starts over.
        let old = self.graph.take().filter(|_| job.focus.is_some());
        let (opened, explored, back) = old.as_ref().map_or_else(Default::default, |g| (g.opened.clone(), g.explored.clone(), g.back.clone()));
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
        let clusters = match ops::clusters(&self.lib, refs, column) {
            Ok(c) => c,
            Err(e) => return self.status = format!("{e:#}"),
        };
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
        let password = if self.lib.remotes.contains_key(&location) {
            match self.remote_secret(&location, Retry::Command(command.into())) {
                Some(p) => Some(p),
                None => return,
            }
        } else {
            None
        };
        let targets = self.targets();
        if !self.confirm_many("fetch PDFs for", targets.iter().filter(|&&i| self.lib.refs[i].pdfs.is_empty()).count(), command) {
            return;
        }
        let openalex_key = self.openalex_key();
        let request = ops::fetch_request(&self.lib, targets.iter().map(|&i| &self.lib.refs[i]), true, openalex_key);
        if request.items.is_empty() {
            self.status = if targets.is_empty() { "no reference selected" } else { "they already have PDFs" }.into();
            return;
        }
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
            ops::check_download(&self.lib.refs[i], file)?;
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
        let (text, format) = match ops::cite_text(&self.lib, &self.dir, &refs, format) {
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

/// Splits on whitespace, except inside double quotes, which are dropped: `--to "My Thesis"`.
fn words(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = args.trim_start();
    while !rest.is_empty() {
        let (word, after) = match rest.strip_prefix('"').and_then(|r| r.split_once('"')) {
            Some((quoted, after)) => (quoted, after),
            None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
        };
        out.push(word);
        rest = after.trim_start();
    }
    out
}

/// Splits a command's `--name value` and `--name=value` options, which may come anywhere, from the
/// rest of its arguments; `--` ends the options. Only `known` names are accepted, and `flags` take no
/// value. A name given twice keeps every value, in order.
fn options<'a>(args: &'a str, known: &[&str], flags: &[&str]) -> anyhow::Result<(HashMap<&'a str, Vec<&'a str>>, String)> {
    let (mut opts, mut rest): (HashMap<&str, Vec<&str>>, _) = (HashMap::new(), Vec::new());
    let mut words = words(args).into_iter();
    while let Some(word) = words.next() {
        let Some(name) = word.strip_prefix("--") else {
            rest.push(word);
            continue;
        };
        if name.is_empty() {
            rest.extend(words);
            break;
        }
        if flags.contains(&name) {
            opts.entry(name).or_default();
            continue;
        }
        let (name, value) = name.split_once('=').map_or_else(|| (name, words.next()), |(n, v)| (n, Some(v)));
        if !known.contains(&name) {
            let all = known.iter().chain(flags).map(|k| format!("--{k}")).collect::<Vec<_>>().join(", ");
            anyhow::bail!("unknown option --{name}; this command takes {all}");
        }
        opts.entry(name).or_default().push(value.filter(|v| !v.is_empty()).ok_or_else(|| anyhow::anyhow!("--{name} needs a value"))?);
    }
    Ok((opts, rest.join(" ")))
}

/// The status line after :discover, with the search note.
fn discover_status(found: &[Candidate], note: Option<&str>) -> String {
    let status = match found.len() {
        0 => "discover: no papers found".to_string(),
        n => format!("discover: {n} papers, {} found in OpenAlex", found.iter().filter(|c| c.work.is_some()).count()),
    };
    match note {
        Some(note) => format!("{status} ({note})"),
        None => status,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn discover_status_says_when_the_web_was_not_searched() {
        let found = yeda_core::discover::Found { title: "Swarms".into(), authors: "Doe".into(), year: None, doi: None, url: None, why: String::new() };
        let found = [super::Candidate { found, work: None, text: None }];
        let note = "web search: keenable (exa: rate limited)";
        assert_eq!(super::discover_status(&found, Some(note)), format!("discover: 1 papers, 0 found in OpenAlex ({note})"));
        assert_eq!(super::discover_status(&found, None), "discover: 1 papers, 0 found in OpenAlex");
        assert_eq!(super::discover_status(&[], Some(note)), format!("discover: no papers found ({note})"));
    }

    use ratatui::crossterm::event::KeyCode;

    fn plain_app() -> super::App {
        let lib = yeda_core::library::Library::parse(None, None, [("a".to_string(), "title = \"A\"\n".to_string())], None).unwrap();
        super::App::new(std::path::PathBuf::from("/tmp"), lib, ratatui_image::picker::Picker::halfblocks(), crate::keys::Keymap::load(None).unwrap())
    }

    #[test]
    fn add_to_queues_the_ids_for_a_collection_existing_references_join() {
        let mut app = plain_app();
        app.submit(super::Prompt::Command, "add --to PhD 10.1145/3442188.3445922 arXiv:2401.00001");
        let Some(super::Job::Fetch(batch)) = app.job.take() else { panic!("no add job") };
        assert_eq!((batch.collection.as_deref(), batch.join_existing, batch.todo.len()), (Some("PhD"), true, 2));
        app.submit(super::Prompt::Command, "add --to PhD");
        assert!(app.job.is_none() && app.status.starts_with("usage: :add --to"), "{}", app.status);
    }

    #[test]
    fn a_plan_runs_one_step_at_a_time_and_marks_what_add_found() {
        let mut app = plain_app();
        app.plan = Some(yeda_core::assist::Plan {
            steps: vec![
                yeda_core::assist::Step { action: yeda_core::assist::Action::Add { ids: vec!["10.1/x".into()], to: Some("PhD".into()) }, reason: String::new() },
                yeda_core::assist::Step { action: yeda_core::assist::Action::Fetch { location: None }, reason: String::new() },
            ],
            reply: String::new(),
        });
        app.mode = super::Mode::Plan;
        app.plan_key(ratatui::crossterm::event::KeyEvent::from(KeyCode::Enter));
        app.poll_plan();
        let Some(super::Job::Fetch(mut batch)) = app.job.take() else { panic!("no add job") };
        assert!(batch.mark_result);
        app.job = Some(super::Job::Fetch(super::Batch { todo: Default::default(), ..batch.clone() }));
        app.poll_plan();
        assert_eq!(app.plan_queue.len(), 1, "the next step waits for the add to finish");
        app.job = None;
        batch.existed.push("a".into());
        app.finish_batch(batch);
        assert_eq!(app.marked.iter().collect::<Vec<_>>(), ["a"]);
        assert_eq!(app.plan_queue.len(), 1);
    }

    #[test]
    fn do_logs_its_plan_failures_and_steps() {
        let path = std::env::temp_dir().join(format!("yeda-do-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = plain_app();
        app.log.path = Some(path.clone());
        let send = |app: &mut super::App, answer: &str| {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Ok(answer.to_string())).unwrap();
            app.assist_job = Some(rx);
            app.poll_assist();
        };
        send(&mut app, r#"{"steps": [], "reply": "No command filters the list."}"#);
        send(&mut app, "not a\nplan");
        app.assist_request = "add 10.1/x to PhD and fetch it".into();
        send(&mut app, r#"{"steps": [{"action": {"command": "add", "ids": ["10.1/x"], "to": "PhD"}, "reason": "files it"}, {"action": {"command": "fetch", "location": null}, "reason": "gets the PDF"}], "reply": ""}"#);
        app.plan_key(ratatui::crossterm::event::KeyEvent::from(KeyCode::Enter));
        app.poll_plan();
        app.job = None;
        app.plan_queue.clear();
        app.poll_plan();
        let log = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = log.lines().map(|l| l.split_once(' ').unwrap().1).collect();
        assert_eq!(lines[0], "do: no plan: No command filters the list.");
        assert!(lines[1].starts_with("do: failed: ") && lines[1].ends_with("answer: not a plan"), "{}", lines[1]);
        assert_eq!(lines[2], "do: plan 1/2: add --to PhD 10.1/x — files it");
        assert_eq!(lines[3], "do: plan 2/2: fetch — gets the PDF");
        assert_eq!(lines[4], "do: approved");
        assert_eq!(lines[5], "do: step 1/2: add --to PhD 10.1/x");
        assert_eq!(lines[6], "do: done");
    }

    #[test]
    fn search_sets_the_library_query() {
        let mut app = plain_app();
        app.submit(super::Prompt::Command, "search title:a");
        assert_eq!((app.query.as_str(), app.visible.len()), ("title:a", 1));
        app.submit(super::Prompt::Command, "search title:zzz");
        assert_eq!(app.visible.len(), 0);
    }

    #[test]
    fn a_plan_stops_when_add_finds_nothing() {
        let mut app = plain_app();
        app.plan_queue = ["fetch".to_string()].into();
        app.submit(super::Prompt::Command, "add --to PhD 10.1/x");
        let Some(super::Job::Fetch(batch)) = app.job.take() else { panic!("no add job") };
        app.finish_batch(batch);
        assert!(app.plan_queue.is_empty());
    }

    #[test]
    fn quotes_keep_an_option_value_whole() {
        let (opts, rest) = super::options(r#"--to "My Thesis" 10.1/x --llm a"#, &["to", "llm"], &[]).unwrap();
        assert_eq!((opts["to"].as_slice(), opts["llm"].as_slice(), rest.as_str()), (&["My Thesis"][..], &["a"][..], "10.1/x"));
    }

    #[test]
    fn splits_options_from_text() {
        let (opts, rest) = super::options("swarm --llm claude robots", &["llm"], &[]).unwrap();
        assert_eq!((opts["llm"].as_slice(), rest.as_str()), (&["claude"][..], "swarm robots"));
        let (opts, rest) = super::options("--llm=gpt -- --not an option", &["llm"], &[]).unwrap();
        assert_eq!((opts["llm"].as_slice(), rest.as_str()), (&["gpt"][..], "--not an option"));
        assert!(super::options("--llm", &["llm"], &[]).is_err());
        assert!(super::options("--model x", &["llm"], &[]).is_err());
        let (opts, rest) = super::options("--like --llm a swarm --llm=b", &["llm"], &["like"]).unwrap();
        assert_eq!((opts["llm"].as_slice(), opts.contains_key("like"), rest.as_str()), (&["a", "b"][..], true, "swarm"));
    }

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
    fn envvars_name_the_variables_and_the_secrets_needing_an_alias() {
        let toml = "[providers.a]\nbase_url = \"https://api.example.com/v1\"\nprotocol = \"openai\"\nauth = \"api-key\"\n\
                    [providers.b]\nbase_url = \"https://api.example.com/v2\"\nprotocol = \"openai\"\nauth = \"api-key\"\n\
                    [providers.c]\nbase_url = \"https://other.example.com\"\nprotocol = \"openai\"\nauth = \"api-key\"\nalias = \"c\"\n\
                    [providers.d]\nbase_url = \"https://x.alias\"\nprotocol = \"openai\"\nauth = \"api-key\"\n";
        let lib = yeda_core::library::Library::parse(Some(toml), None, [], None).unwrap();
        let mut app = super::App::new(std::path::PathBuf::from("/tmp"), lib, ratatui_image::picker::Picker::halfblocks(), crate::keys::Keymap::load(None).unwrap());
        app.open_envvars();
        let text = |name: &str| app.secret_items.iter().find(|i| i.label.starts_with(&format!("Provider {name}:"))).unwrap().status.clone();
        assert!(matches!(app.mode, super::Mode::Envvars));
        assert_eq!(text("a"), "$YEDA_KEY_API_EXAMPLE_COM_443\nneeds an alias: 2 secrets on this server share this variable");
        assert!(text("b").contains("needs an alias"));
        assert_eq!(text("c"), "$YEDA_KEY_OTHER_EXAMPLE_COM_443_ALIAS_C\n$YEDA_KEY_OTHER_EXAMPLE_COM_443");
        assert!(text("d").starts_with("needs an alias: without one"), "{}", text("d"));
        app.secret_sel = app.secret_items.iter().position(|i| i.label.starts_with("Provider c:")).unwrap();
        assert_eq!(app.selected_envvar().as_deref(), Some("YEDA_KEY_OTHER_EXAMPLE_COM_443_ALIAS_C"));
        app.secret_sel = app.secret_items.iter().position(|i| i.label.starts_with("Provider d:")).unwrap();
        assert_eq!(app.selected_envvar(), None);
    }

    #[test]
    fn esc_clears_the_focused_column_then_every_search() {
        use super::Pane;
        use yeda_core::browse::{Sort, SortField};
        let refs = [("a2020", "title = \"Alpha\"\nyear = 2020\n"), ("b2019", "title = \"Beta\"\nyear = 2019\n")];
        let lib = yeda_core::library::Library::parse(None, None, refs.map(|(k, t)| (k.to_string(), t.to_string())), None).unwrap();
        let mut app = super::App::new(std::path::PathBuf::from("/tmp"), lib, ratatui_image::picker::Picker::halfblocks(), crate::keys::Keymap::load(None).unwrap());
        let sort = Sort { field: SortField::Year, descending: true };
        app.sort = sort;
        let searches = |app: &mut super::App| {
            app.query = "alpha".into();
            app.side_query = "x".into();
            app.refilter_sidebar(false);
            assert_eq!(app.visible.len(), 1);
        };

        searches(&mut app);
        app.pane = Pane::Sidebar;
        app.action("clear", "");
        assert_eq!((app.side_query.as_str(), app.query.as_str()), ("", "alpha"));
        app.action("clear", "");
        assert_eq!((app.query.as_str(), app.visible.len()), ("", 2));

        searches(&mut app);
        app.pane = Pane::List;
        app.action("clear", "");
        assert_eq!((app.side_query.as_str(), app.query.as_str()), ("x", ""));
        app.action("clear", "");
        assert_eq!(app.side_query, "");

        searches(&mut app);
        app.table = Some(crate::table::TableView::new("t".into(), vec!["a2020".into()]));
        app.right = super::RightView::Tables;
        app.pane = Pane::Right;
        let t = app.table.as_mut().unwrap();
        t.query = "a".into();
        t.filters.push((0, "a".into()));
        app.action("table-clear", "");
        let t = app.table.as_ref().unwrap();
        assert_eq!((t.query.as_str(), t.filters.len(), app.query.as_str()), ("", 0, "alpha"));
        app.action("table-clear", "");
        assert_eq!((app.query.as_str(), app.side_query.as_str(), app.table.is_some()), ("", "", true));
        app.action("table-clear", "");
        assert!(app.table.is_none());
        assert_eq!(app.sort, sort);
    }

    #[test]
    fn renaming_a_provider_moves_its_profiles() {
        let mut root: toml::Table = toml::from_str("[providers.a]\nbase_url = \"https://x.example\"\n[llm.m]\nprovider = \"a\"\nmodel = \"1\"\n[llm.n]\nprovider = \"b\"\nmodel = \"2\"\n").unwrap();
        super::rename_provider(&mut root, "a", "z");
        assert!(root["providers"].as_table().unwrap().is_empty());
        assert_eq!((root["llm"]["m"]["provider"].as_str(), root["llm"]["n"]["provider"].as_str()), (Some("z"), Some("b")));
    }

    #[test]
    fn the_setup_picker_lists_entries_and_offers_a_new_one() {
        let lib = yeda_core::library::Library::parse(Some("default_llm = \"a\"\n[providers.p]\nbase_url = \"https://x.example\"\nprotocol = \"openai\"\nauth = \"none\"\n[llm.a]\nprovider = \"p\"\nmodel = \"m\"\n"), None, [], None).unwrap();
        let mut app = super::App::new(std::path::PathBuf::from("/tmp"), lib, ratatui_image::picker::Picker::halfblocks(), crate::keys::Keymap::load(None).unwrap());
        app.submit(super::Prompt::Command, "setup llm");
        assert!(app.mode == super::Mode::SetupPick);
        assert_eq!(app.setup_picker.as_ref().unwrap().options.len(), 1);
        app.input = "b".into();
        app.refresh_setup_picker();
        let options = &app.setup_picker.as_ref().unwrap().options;
        assert!(options.last().is_some_and(|o| o.new && o.name == "b"));
        app.submit(super::Prompt::Command, "setup llm extra");
        assert!(app.status.starts_with("usage: :setup"));
    }

    #[test]
    fn combo_keys_are_nested_inside_the_setup_form() {
        let lib = yeda_core::library::Library::parse(Some("[providers.codex]\nbase_url = \"https://api.openai.com/v1\"\nprotocol = \"openai-responses\"\nauth = \"oauth\"\n"), None, [], None).unwrap();
        let form = super::SetupForm::new(super::setup::Kind::Provider, "", &lib);
        assert_eq!(form.values, [""; 7], "a new provider form starts empty");
        let mut app = super::App::new(
            std::path::PathBuf::from("/tmp"),
            lib,
            ratatui_image::picker::Picker::halfblocks(),
            crate::keys::Keymap::load(None).unwrap(),
        );
        app.mode = super::Mode::Setup;
        app.setup = Some(form);
        let key = |code| ratatui::crossterm::event::KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE);
        use ratatui::crossterm::event::KeyCode;

        app.setup_key(key(KeyCode::Tab));
        {
            let c = app.combo.as_ref().unwrap();
            assert!(c.open);
            assert_eq!(c.options.len(), 7, "all options show on focus");
            assert_eq!(c.options[c.sel].1, "codex", "the first preset is highlighted");
        }
        app.setup_key(key(KeyCode::Esc));
        assert!(!app.combo.as_ref().unwrap().open, "Esc closes the dropdown");
        assert_eq!(app.setup.as_ref().unwrap().values[1], "", "Esc keeps the field empty");
        assert!(app.setup.is_some(), "Esc does not close the form yet");

        app.setup_key(key(KeyCode::Tab));
        app.setup_key(key(KeyCode::BackTab));
        assert!(app.combo.as_ref().unwrap().open);
        assert!(app.combo.as_ref().unwrap().query.is_empty(), "the filter starts empty");
        for ch in "clau".chars() {
            app.setup_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.setup.as_ref().unwrap().values[1], "clau");
        assert_eq!(app.combo.as_ref().unwrap().options[0].1, "claude");
        app.setup_key(key(KeyCode::Enter));
        let form = app.setup.as_ref().unwrap();
        assert_eq!(form.values[1], "claude");
        assert_eq!(form.values[3], "anthropic", "picking a preset fills its protocol");
        assert!(!app.combo.as_ref().unwrap().open, "Enter picks the value and closes the dropdown");

        app.setup_key(key(KeyCode::Tab));
        assert_eq!(app.setup.as_ref().unwrap().sel, 2);
        assert!(app.combo.is_none(), "fields without choices have no dropdown");
        app.setup_key(key(KeyCode::Esc));
        assert!(app.setup.is_none(), "Esc closes the form once the dropdown is gone");
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(super::unquote("\"~/a b.pdf\""), "~/a b.pdf");
        assert_eq!(super::unquote("'a.pdf'"), "a.pdf");
        assert_eq!(super::unquote("~/a b.pdf"), "~/a b.pdf");
        assert_eq!(super::unquote("\"a.pdf'"), "\"a.pdf'");
    }
}
