use crate::app::{ACTIONS, App, GraphTab, COMMANDS, FilterForm, GUIDE, HelpPage, Mode, NOTES, Pane, PickKind, Prompt, RightView};
use crate::keys::{self, Keymap};
use yeda_core::browse::SortField;
use yeda_core::search::FILTER_FIELDS;
use yeda_core::annotations::Annotation;
use yeda_core::browse::Filter;
use yeda_core::library::{Library, Ref};
use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_image::StatefulImage;
use std::collections::HashMap;
use yeda_core::graph::Graph;

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused { Style::new().fg(Color::Cyan) } else { Style::new() };
    Block::bordered().title(title).border_style(style)
}

/// Records a bordered list (with its scroll offset) or column under `pane`, for mouse clicks.
fn clickable(app: &mut App, pane: Pane, area: Rect, offset: Option<usize>) {
    app.clickable.push((pane, area.inner(layout::Margin::new(1, 1)), offset));
}

fn color(c: &str) -> Option<Color> {
    c.parse().ok()
}

fn tag_style(lib: &Library, tag: &str) -> Style {
    lib.tag_colors.get(tag).and_then(|c| color(c)).map_or(Style::new(), |c| Style::new().fg(c))
}

fn label(f: &Filter, lib: &Library) -> Line<'static> {
    match f {
        Filter::All => Line::from("All"),
        Filter::Search(name, _) => Line::from(format!("⌕ {name}")),
        Filter::Collection(c) => {
            let depth = c.matches('/').count();
            Line::from(format!("{}{}", "  ".repeat(depth), c.rsplit('/').next().unwrap()))
        }
        Filter::Tag(t) => Line::styled(format!("#{t}"), tag_style(lib, t)),
    }
}

pub fn status_glyph(status: Option<&str>) -> &'static str {
    match status {
        None => " ",
        Some("New") => "✦",
        Some("To Read") => "○",
        Some("In Progress") => "◐",
        Some("Read") => "●",
        Some("Not Reading") => "✕",
        Some(_) => "?",
    }
}

fn row(r: &Ref, lib: &Library, marked: bool) -> Line<'static> {
    let year = r.year.map(|y| y.to_string()).unwrap_or_default();
    let author = r.authors.first().and_then(|a| a.split(',').next()).unwrap_or("");
    let mark = if marked { Span::styled("▌", Style::new().fg(Color::Yellow)) } else { Span::raw(" ") };
    let mut spans = vec![mark, Span::raw(format!("{} {year:4}  {author:<16.16} ", status_glyph(r.status.as_deref())))];
    for t in r.tags.iter().filter(|t| lib.tag_colors.contains_key(*t)) {
        spans.push(Span::styled("●", tag_style(lib, t)));
    }
    spans.push(Span::raw(format!(" {}", r.title)));
    Line::from(spans)
}

fn field(lines: &mut Vec<Line<'static>>, name: &str, value: Vec<Span<'static>>) {
    if value.iter().any(|s| !s.content.is_empty()) {
        lines.push(Line::from([vec![Span::styled(format!("{name:<12}"), Style::new().dim())], value].concat()));
    }
}

fn details(r: &Ref, lib: &Library) -> Text<'static> {
    let mut lines = vec![Line::from(r.title.clone()).bold(), Line::default()];
    let text = |s: String| vec![Span::raw(s)];
    field(&mut lines, "key", text(r.key.clone()));
    field(&mut lines, "type", text(r.kind.clone()));
    field(&mut lines, "authors", text(r.authors.join("; ")));
    field(&mut lines, "year", text(r.year.map(|y| y.to_string()).unwrap_or_default()));
    field(&mut lines, "doi", text(r.doi.clone().unwrap_or_default()));
    field(&mut lines, "status", text(r.status.clone().unwrap_or_default()));
    let tags = r.tags.iter().enumerate().flat_map(|(i, t)| {
        [Span::raw(if i > 0 { ", " } else { "" }), Span::styled(t.clone(), tag_style(lib, t))]
    });
    field(&mut lines, "tags", tags.collect());
    field(&mut lines, "collections", text(r.collections.join(", ")));
    field(&mut lines, "pdf", text(r.pdf.as_ref().map(|p| format!("{}:{}", p.location, p.path)).unwrap_or_default()));
    for (k, v) in &r.extra {
        field(&mut lines, k, text(v.as_str().map(String::from).unwrap_or_else(|| v.to_string())));
    }
    Text::from(lines)
}

fn annotation_text(a: &Annotation) -> Text<'static> {
    let mut lines = vec![Line::from(Span::styled(format!("{} · p. {}", a.kind, a.page), Style::new().dim()))];
    if let Some(t) = &a.text {
        let bar = Style::new().fg(a.color.as_deref().and_then(color).unwrap_or(Color::Yellow));
        lines.extend(t.lines().map(|l| Line::from(vec![Span::styled("▌ ", bar), Span::raw(l.to_string()).italic()])));
    }
    if let Some(c) = &a.comment {
        lines.push(Line::default());
        lines.extend(c.lines().map(|l| Line::from(l.to_string())));
    }
    Text::from(lines)
}

fn draw_annotations(f: &mut Frame, app: &mut App, area: Rect) {
    app.sync_annots();
    let focused = app.pane == Pane::Right;
    let [top, bottom] = Layout::vertical([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);
    let items: Vec<ListItem> = app
        .annots
        .iter()
        .map(|a| {
            let color = a.color.as_deref().and_then(color).unwrap_or(Color::Gray);
            let summary = a.text.as_deref().or(a.comment.as_deref()).unwrap_or(&a.kind);
            ListItem::new(Line::from(vec![
                Span::styled("▌", Style::new().fg(color)),
                Span::raw(format!("p.{:<4} {}", a.page, summary.replace('\n', " "))),
            ]))
        })
        .collect();
    let title = format!("Annotations ({})", app.annots.len());
    let mut state = ListState::default().with_selected(Some(app.annot_sel));
    f.render_stateful_widget(
        List::new(items)
            .block(block(title, focused))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        top,
        &mut state,
    );
    clickable(app, Pane::Right, top, Some(state.offset()));

    let outer = block(String::new(), false);
    let inner = outer.inner(bottom);
    f.render_widget(outer, bottom);
    let Some(a) = app.annots.get(app.annot_sel) else {
        f.render_widget(Paragraph::new("No annotations. Press x to extract them from the PDF.").dim(), inner);
        return;
    };
    let text = annotation_text(a);
    if app.image.is_none() {
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        return;
    }
    let [caption, picture] = Layout::vertical([Constraint::Length(text.height() as u16), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), caption);
    if let Some(state) = &mut app.image {
        f.render_stateful_widget(StatefulImage::default(), picture, state);
    }
}

fn draw_tables(f: &mut Frame, app: &mut App, area: Rect) {
    app.sync_table_scroll();
    let mut lines: Vec<Line> = Vec::new();
    if app.lib.tables.is_empty() {
        lines.push(Line::from("No tables. Define [tables.<name>] in yeda.toml or run import-table.").dim());
    }
    if let Some(r) = app.selected() {
        for (name, table) in &app.lib.tables {
            lines.push(Line::from(format!("▸ {name}")).bold());
            let Some(row) = r.tables.get(name) else {
                lines.push(Line::from(format!("  no row yet: :summarize {name}")).dim());
                lines.push(Line::default());
                continue;
            };
            for c in &table.columns {
                lines.push(Line::from(Span::styled(c.clone(), Style::new().fg(Color::Cyan))));
                match row.get(c).filter(|v| !v.trim().is_empty()) {
                    Some(v) => lines.extend(v.lines().map(|l| Line::from(l.to_string()))),
                    None => lines.push(Line::from("—").dim()),
                }
                lines.push(Line::default());
            }
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.table_scroll, 0))
            .block(block("Tables".into(), app.pane == Pane::Right)),
        area,
    );
}

/// Narrower windows drop the right column, then the library column.
const THREE_COLUMNS: u16 = 120;
const TWO_COLUMNS: u16 = 80;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, bar] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let [side, list, detail] = match (app.focus, app.pane) {
        (true, Pane::Sidebar) => [main, Rect::ZERO, Rect::ZERO],
        (true, Pane::List) => [Rect::ZERO, main, Rect::ZERO],
        (true, Pane::Right) => [Rect::ZERO, Rect::ZERO, main],
        (false, _) if main.width >= THREE_COLUMNS => {
            Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(40), Constraint::Percentage(40)]).areas(main)
        }
        (false, _) if main.width >= TWO_COLUMNS => {
            let [side, list] = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(main);
            [side, list, Rect::ZERO]
        }
        (false, _) => [Rect::ZERO, main, Rect::ZERO],
    };
    // A hidden column can't keep the focus; `c` still shows any column on its own.
    if !app.focus && [(Pane::Sidebar, side), (Pane::Right, detail)].iter().any(|&(p, r)| app.pane == p && r.is_empty()) {
        app.pane = Pane::List;
    }
    app.page = main.height.saturating_sub(2) as usize;
    app.clickable.clear();
    clickable(app, Pane::Sidebar, side, None);
    clickable(app, Pane::List, list, None);
    clickable(app, Pane::Right, detail, None);
    let hl = Style::new().add_modifier(Modifier::REVERSED);

    let items: Vec<ListItem> = app.side_visible.iter().map(|&i| ListItem::new(label(&app.sidebar[i], &app.lib))).collect();
    let title = if app.side_query.is_empty() { "Library".into() } else { format!("Library /{}", app.side_query) };
    let mut state = ListState::default().with_selected(Some(app.side_sel));
    f.render_stateful_widget(List::new(items).block(block(title, app.pane == Pane::Sidebar)).highlight_style(hl), side, &mut state);
    clickable(app, Pane::Sidebar, side, Some(state.offset()));

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&i| ListItem::new(row(&app.lib.refs[i], &app.lib, app.marked.contains(&app.lib.refs[i].key))))
        .collect();
    let order = if app.sort.descending { "▼" } else { "▲" };
    let mut title = format!("References ({}) {order} {}", app.visible.len(), app.sort.field.name());
    if !app.marked.is_empty() {
        title += &format!(" · {} marked", app.marked.len());
    }
    if app.visual.is_some() {
        title += " · VISUAL";
    }
    let mut state = ListState::default().with_selected(Some(app.sel));
    f.render_stateful_widget(List::new(items).block(block(title, app.pane == Pane::List)).highlight_style(hl), list, &mut state);
    clickable(app, Pane::List, list, Some(state.offset()));

    if app.right == RightView::Graph && app.graph.is_some() {
        draw_graph(f, app, detail);
    } else if app.right == RightView::Annotations && !detail.is_empty() {
        draw_annotations(f, app, detail);
    } else if app.right == RightView::Tables {
        draw_tables(f, app, detail);
    } else {
        let text = app.selected().map(|r| details(r, &app.lib)).unwrap_or_default();
        f.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(block("Details".into(), false)),
            detail,
        );
    }

    let status = match app.mode {
        Mode::Search => format!("/{}", app.query),
        Mode::SideSearch => format!("library /{}", app.side_query),
        Mode::Prompt(Prompt::TagColor) => format!("color for #{} (#rrggbb or name, empty removes): {}", app.current_filter().name(), app.input),
        Mode::Pick => match &app.chooser {
            Some(c) if c.kind == PickKind::Tags => {
                format!("tags: {} › {}", c.tags.iter().map(|t| format!("[{t}]")).collect::<Vec<_>>().join(" "), app.input)
            }
            _ => format!("move to collection › {}", app.input),
        },
        Mode::Prompt(Prompt::Command) => format!(":{}", app.input),
        Mode::Prompt(Prompt::Add) => format!("add DOIs / arXiv IDs / ISBNs: {}", app.input.replace('\n', "  ")),
        Mode::Prompt(Prompt::Secret) => format!(
            "secret for {} (Enter on empty cancels): {}",
            app.asking.as_ref().map_or("", |p| p.label.as_str()),
            "•".repeat(app.input.chars().count())
        ),
        _ if !app.status.is_empty() => app.status.clone(),
        _ if !app.query.is_empty() => format!("/{}", app.query),
        _ => String::new(),
    };
    f.render_widget(Paragraph::new(status), bar);
    let anchor = if list.is_empty() { main } else { list };
    if let Some(c) = &app.chooser {
        let height = (c.options.len().min(10) as u16 + 2).min(bar.y);
        let area = Rect { x: anchor.x, y: bar.y - height, width: anchor.width, height };
        let hint = if c.kind == PickKind::Tags {
            " Enter toggle · Tab/↓ next · Esc done "
        } else {
            " Enter move · Tab/↓ next · Esc cancel "
        };
        let items: Vec<ListItem> = c.options.iter().map(|(label, _)| ListItem::new(label.clone())).collect();
        let sel = c.active(&app.input).then_some(c.sel);
        f.render_widget(Clear, area);
        f.render_stateful_widget(
            List::new(items).block(block(hint.into(), true)).highlight_style(hl),
            area,
            &mut ListState::default().with_selected(sel),
        );
    }
    match app.mode {
        Mode::Filter => draw_filter(f, app, main),
        Mode::Sort => draw_sort(f, app, main),
        Mode::Help(page) => draw_help(f, app, main, page),
        _ => {}
    }
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(layout::Flex::Center).areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))]).flex(layout::Flex::Center).areas(area);
    area
}

fn draw_filter(f: &mut Frame, app: &App, main: Rect) {
    let Some(form) = &app.form else { return };
    let labels = FILTER_FIELDS.iter().map(|(_, label)| *label).chain(["Text"]);
    let mut lines: Vec<Line> = labels
        .zip(&form.values)
        .enumerate()
        .map(|(i, (label, value))| {
            let cursor = if i == form.sel { "▏" } else { "" };
            let line = Line::from(vec![Span::raw(format!("{label:>10}  ")).bold(), Span::raw(format!("{value}{cursor}"))]);
            if i == form.sel { line.reversed() } else { line }
        })
        .collect();
    let suggestions = form.suggestions();
    if !suggestions.is_empty() {
        lines.push(Line::raw(format!("{:>10}  Ctrl-N/Ctrl-P pick · → completes", "")).dim());
    }
    for (i, s) in suggestions.iter().enumerate() {
        let line = Line::raw(format!("{:>10}  {s}", ""));
        lines.push(if i == form.pick { line.reversed() } else { line.dim() });
    }
    let height = (FILTER_FIELDS.len() + 2 + FilterForm::SHOWN) as u16 + 2;
    let area = popup(main, 60, height);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block(" Filter · Tab/↓ next · Enter apply · Esc cancel ".into(), true)), area);
}

fn draw_sort(f: &mut Frame, app: &App, main: Rect) {
    let items: Vec<ListItem> = SortField::ALL
        .iter()
        .map(|&field| {
            let mark = match (field == app.sort.field, app.sort.descending) {
                (true, false) => "▲ ",
                (true, true) => "▼ ",
                _ => "  ",
            };
            ListItem::new(format!("{mark}{}", field.name()))
        })
        .collect();
    let area = popup(main, 44, items.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_stateful_widget(
        List::new(items)
            .block(block(" Sort · Enter again reverses ".into(), true))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut ListState::default().with_selected(Some(app.sort_sel)),
    );
}

/// The keymap as it is, grouped by command, then the commands.
fn key_help(keymap: &Keymap) -> Vec<Line<'static>> {
    let describe = |command: &str| {
        let name = command.split_whitespace().next().unwrap_or_default();
        let found = ACTIONS.iter().find(|(a, _)| *a == name).map(|(_, d)| *d);
        let found = found.or_else(|| COMMANDS.iter().find(|(c, _)| c.trim_start_matches(':').split([' ', '[', '!']).next() == Some(name)).map(|(_, d)| *d));
        found.map_or_else(|| format!(":{command}"), |d| if command.contains(' ') { format!("{d} (:{command})") } else { d.to_string() })
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    for context in keys::CONTEXTS {
        let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
        for (seq, command) in keymap.bindings(context) {
            let key: String = seq.concat();
            match grouped.iter_mut().find(|(c, _)| c == command) {
                Some((_, keys)) => keys.push(key),
                None => grouped.push((command.clone(), vec![key])),
            }
        }
        if grouped.is_empty() {
            continue;
        }
        rows.push((String::new(), String::new()));
        rows.push((format!("[{context}]"), if context == "normal" { "everywhere".into() } else { format!("in the {context} column") }));
        rows.extend(grouped.into_iter().map(|(command, keys)| (keys.join(" "), describe(&command))));
    }
    rows.push((String::new(), String::new()));
    rows.extend(COMMANDS.iter().chain(NOTES).map(|(k, v)| (k.to_string(), v.to_string())));
    let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    rows.into_iter()
        .skip(1)
        .map(|(k, v)| {
            let style = if k.starts_with('[') { Style::new().fg(Color::Cyan).bold() } else { Style::new().fg(Color::Cyan) };
            Line::from(vec![Span::styled(format!("{k:<width$}  "), style), Span::raw(v)])
        })
        .collect()
}

fn draw_help(f: &mut Frame, app: &mut App, main: Rect, page: HelpPage) {
    let (title, lines): (&str, Vec<Line>) = match page {
        HelpPage::Keys => (" Keys · j/k scroll · q close · :help for the guide ", key_help(&app.keymap)),
        HelpPage::Guide => {
            // Headings are the unindented lines.
            let lines = GUIDE
                .lines()
                .map(|l| if l.starts_with(' ') || l.is_empty() { Line::from(l) } else { Line::from(l).bold().fg(Color::Cyan) })
                .collect();
            (" Guide · j/k Ctrl-d/u scroll · q close ", lines)
        }
        HelpPage::Failed => (if app.report_copied { " Failed · copied to the clipboard · q close " } else { " Failed · q close " }, app.report.iter().map(|l| Line::from(l.as_str())).collect()),
    };
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 2;
    let area = popup(main, width, lines.len() as u16 + 2);
    app.help_scroll = app.help_scroll.min((lines.len() as u16).saturating_sub(area.height.saturating_sub(2)));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).scroll((app.help_scroll, 0)).block(block(title.into(), true)), area);
}

/// Rows by year, newest at the top; ◉ seeds, ● other library papers, ○ papers outside it. The
/// selected paper is yellow, its neighbours cyan, and the neighbour highlighted in the list a magenta ◆.
/// Unused for edges away from the selection, which would fill a large graph; the selected
/// paper's darker, the picked neighbour's brightest.
const EDGE_COLORS: [Color; 3] = [Color::DarkGray, Color::Yellow, Color::LightYellow];

/// Text rows a year may take before its least important nodes lose their labels.
const MAX_LANES: usize = 4;

#[derive(Clone, Copy, PartialEq)]
enum Label {
    None,
    Right,
    /// Near the right edge.
    Left,
}

struct Placement {
    /// Node → its text row and where its label goes.
    at: HashMap<usize, (usize, Label)>,
    /// Each year's first row.
    years: Vec<(usize, Option<i32>)>,
    rows: usize,
}

/// Splits years into lanes, filled most important node first, so labels never overlap.
fn place(g: &Graph, shown: &[usize], rank: &dyn Fn(usize) -> u8, col: &dyn Fn(usize) -> i32, width: i32) -> Placement {
    let mut out = HashMap::new();
    let mut starts = Vec::new();
    let mut next = 0;
    for (row, year) in g.years.iter().enumerate() {
        let mut members: Vec<usize> = shown.iter().copied().filter(|&i| g.nodes[i].row == row).collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|&i| (std::cmp::Reverse(rank(i)), col(i)));
        let mut lanes: Vec<Vec<(i32, i32)>> = Vec::new();
        let free = |lane: &[(i32, i32)], (a, b): (i32, i32)| lane.iter().all(|&(s, e)| b <= s || e <= a);
        for i in members {
            let (c, len) = (col(i), node_label(&g.nodes[i]).chars().count() as i32);
            // A cell of margin on each side absorbs rounding in where the canvas prints.
            let (text, side) = if c + 3 + len <= width { ((c - 1, c + 4 + len), Label::Right) } else { ((c - 3 - len, c + 2), Label::Left) };
            let glyph = (c - 1, c + 2);
            let (lane, label) = match lanes.iter().position(|l| free(l, text)) {
                Some(l) => (l, side),
                None if lanes.len() < MAX_LANES => {
                    lanes.push(Vec::new());
                    (lanes.len() - 1, side)
                }
                None => (lanes.iter().position(|l| free(l, glyph)).unwrap_or(0), Label::None),
            };
            lanes[lane].push(if label == Label::None { glyph } else { text });
            out.insert(i, (next + lane, label));
        }
        starts.push((next, *year));
        next += lanes.len();
    }
    Placement { at: out, years: starts, rows: next }
}

/// Seeds ⊚, other library papers ◎, outside papers ○.
fn marker(n: &yeda_core::graph::Node) -> &'static str {
    if n.seed {
        "⊚"
    } else if n.key.is_some() {
        "◎"
    } else {
        "○"
    }
}

fn node_label(n: &yeda_core::graph::Node) -> String {
    format!("{} {}", n.first_author, n.year.map(|y| y.to_string()).unwrap_or_default())
}

fn draw_graph(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height < 10 || app.graph.is_none() {
        return;
    }
    let focused = app.pane == Pane::Right;
    let v = app.graph.as_mut().unwrap();
    let near = v.entries();
    let list_height = (near.len() as u16 + 3).clamp(5, area.height / 2);
    let [top, bottom] = Layout::vertical([Constraint::Min(6), Constraint::Length(list_height)]).areas(area);
    let g = &v.graph;
    let picked = near.get(v.nbr).map(|&(_, j)| j).filter(|_| focused);
    let shown: Vec<usize> = if v.ego {
        let (citing, cited) = g.neighbours(v.sel);
        let mut s: Vec<usize> = std::iter::once(v.sel).chain(citing).chain(cited).filter(|&i| !g.nodes[i].hidden).collect();
        s.sort();
        s.dedup();
        s
    } else {
        (0..g.nodes.len()).filter(|&i| !g.nodes[i].hidden).collect()
    };
    let rank = |i: usize| match i {
        _ if i == v.sel => 4,
        _ if Some(i) == picked => 3,
        _ if near.iter().any(|&(_, j)| j == i) => 2,
        _ if g.nodes[i].seed => 1,
        _ => 0,
    };
    let ranks: Vec<u8> = (0..g.nodes.len()).map(rank).collect();
    let rank = |i: usize| ranks[i];
    let width = top.width.saturating_sub(2).max(1) as f64;
    // As the canvas maps x to a column.
    let col = |i: usize| ((g.nodes[i].x + 0.15) / 1.15 * (width - 1.0)).round() as i32;
    let Placement { at: placed, years: starts, rows: total } = place(g, &shown, &rank, &col, width as i32);
    let height = top.height.saturating_sub(2).max(1) as usize;
    // Follows the paper picked in the list, and otherwise stays where Ctrl-e/Ctrl-y put it.
    let target = picked.unwrap_or(v.sel);
    if v.scroll_for != Some(target) {
        if let Some(&(r, _)) = placed.get(&target) {
            v.scroll = v.scroll.min(r).max((r + 1).saturating_sub(height));
        }
        v.scroll_for = Some(target);
    }
    v.scroll = v.scroll.min(total.saturating_sub(height));
    let v = app.graph.as_ref().unwrap();
    let g = &v.graph;
    let rows = height as f64;
    // The canvas spreads y over height - 1 rows, so these bounds put text row r exactly on screen row r.
    let y = |r: usize| rows - 1.0 - (r as f64 - v.scroll as f64);
    let pos = |i: usize| (g.nodes[i].x, y(placed[&i].0));
    let is_shown = |i: usize| placed.contains_key(&i);
    // Two character cells, in canvas units, between a node and its label.
    let gap = 2.0 * 1.15 / (width - 1.0);
    let edge = |(a, b): (usize, usize), color| {
        let ((x1, y1), (x2, y2)) = (pos(a), pos(b));
        CanvasLine { x1, y1, x2, y2, color }
    };
    let edges: Vec<(usize, usize)> = g.edges.iter().copied().filter(|&(a, b)| is_shown(a) && is_shown(b)).collect();
    let touches = |(a, b): (usize, usize), i: usize| a == i || b == i;
    // 2: the edge to the picked neighbour, 1: the selected paper's other edges, 0: the rest.
    let edge_level = |e: (usize, usize)| match () {
        _ if touches(e, v.sel) && picked.is_some_and(|p| touches(e, p)) => 2,
        _ if touches(e, v.sel) => 1,
        _ => 0,
    };
    let mut title = format!("Graph of {} ({} papers)", v.scope, g.nodes.iter().filter(|n| !n.hidden).count());
    if v.ego {
        title += &format!(" · neighbourhood: {} · N: all", shown.len());
    } else {
        title += " · N: neighbourhood";
    }
    if total > height {
        title += &format!(" · rows {}–{} of {} · Ctrl-e/y", v.scroll + 1, (v.scroll + height).min(total), total);
    }
    let canvas = Canvas::default()
        .block(block(title, focused))
        .marker(Marker::Braille)
        .x_bounds([-0.15, 1.0])
        .y_bounds([0.0, (rows - 1.0).max(1.0)])
        .paint(|ctx| {
            // The picked neighbour's edge on a later layer, so it crosses over the others.
            for (level, &color) in EDGE_COLORS.iter().enumerate().skip(1) {
                for &e in edges.iter().filter(|&&e| edge_level(e) == level) {
                    ctx.draw(&edge(e, color));
                }
                ctx.layer();
            }
            ctx.layer();
            for &(r, year) in &starts {
                if (v.scroll..v.scroll + height).contains(&r) {
                    ctx.print(-0.15, y(r), Span::raw(year.map_or("?".to_string(), |y| y.to_string())).dim());
                }
            }
            // Later prints cover earlier ones, so the papers that matter most go last.
            let mut order = shown.clone();
            order.sort_by_key(|&i| ranks[i]);
            for i in order {
                let (r, label) = placed[&i];
                if !(v.scroll..v.scroll + height).contains(&r) {
                    continue;
                }
                let n = &g.nodes[i];
                let (glyph, style) = match ranks[i] {
                    // Not bold: most terminals turn bold ANSI yellow into the bright yellow of the edges.
                    4 => (None, Style::new().fg(Color::Yellow)),
                    3 => (Some("◆"), Style::new().fg(Color::LightYellow).bold()),
                    2 => (None, Style::new().fg(Color::Cyan)),
                    _ => (None, Style::new().dim()),
                };
                let glyph = glyph.unwrap_or(marker(n));
                let (x, y) = pos(i);
                ctx.print(x, y, Span::styled(glyph, style));
                let text = node_label(n);
                match label {
                    Label::Right => ctx.print(x + gap, y, Span::styled(text, style)),
                    Label::Left => ctx.print(x - gap / 2.0 * (text.chars().count() + 1) as f64, y, Span::styled(text, style)),
                    Label::None => {}
                }
            }
        });
    f.render_widget(canvas, top);

    let row = |mark: &str, n: &yeda_core::graph::Node| {
        let year = n.year.map(|y| y.to_string()).unwrap_or_default();
        format!("{mark:<4}{} {year:4} {:>6} {:<12.12} {}", marker(n), n.cited_by, n.first_author, n.title)
    };
    let items: Vec<ListItem> = near.iter().map(|(mark, i)| ListItem::new(row(mark, &g.nodes[*i]))).collect();
    let mut title: Vec<Span> = Vec::new();
    for tab in GraphTab::ALL {
        let span = Span::raw(format!(" {} ", tab.name()));
        title.push(if tab == v.tab { span.reversed() } else { span.dim() });
    }
    let order = if v.by_year { "year" } else { "citations" };
    title.push(Span::raw(format!(" · Tab · by {order} (s) · {} {} ", marker(&g.nodes[v.sel]), node_label(&g.nodes[v.sel]))));
    let mut state = ListState::default().with_selected(focused.then_some(v.nbr));
    let border = if focused { Style::new().fg(Color::Cyan) } else { Style::new() };
    let list = List::new(items).block(Block::bordered().title(Line::from(title)).border_style(border)).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, bottom, &mut state);
    clickable(app, Pane::Right, bottom, Some(state.offset()));
}
