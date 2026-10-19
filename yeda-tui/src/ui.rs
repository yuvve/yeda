use crate::table;
use yeda_core::clusters::Kind;
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
use ratatui::widgets::{Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};
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

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.table_rows();
    let focused = app.pane == Pane::Right;
    let Some(t) = &mut app.table else { return };
    let columns: Vec<String> = table::columns(&app.lib, &t.name).into_iter().map(|c| c.name).collect();
    t.row = t.row.min(rows.len().saturating_sub(1));
    t.col = t.col.min(columns.len().saturating_sub(1));
    let widths: Vec<u16> = columns
        .iter()
        .enumerate()
        .map(|(c, name)| {
            let widest = rows.iter().map(|r| r[c].lines().next().unwrap_or_default().chars().count()).max().unwrap_or(0);
            let cap = if name == "key" { 20 } else { 40 };
            widest.max(name.chars().count()).clamp(4, cap) as u16
        })
        .collect();
    let vis = t.visible(columns.len());
    if !vis.contains(&t.col) {
        t.col = vis.iter().copied().find(|&c| c > t.col).or(vis.last().copied()).unwrap_or(0);
    }
    let at = vis.iter().position(|&c| c == t.col).unwrap_or(0);
    let inner = area.inner(layout::Margin::new(1, 1));
    let fits = |from: usize, to: usize| vis[from..=to].iter().map(|&c| widths[c] + 1).sum::<u16>() <= inner.width;
    t.left = t.left.min(at);
    while t.left > 0 && fits(t.left - 1, at) {
        t.left -= 1;
    }
    while t.left < at && !fits(t.left, at) {
        t.left += 1;
    }
    let mut right = at + 1;
    while right < vis.len() && fits(t.left, right) {
        right += 1;
    }
    let mut title = format!("Table {} ({})", t.name, rows.len());
    if let Some((c, descending)) = t.sort {
        title += &format!(" {} {}", if descending { "▼" } else { "▲" }, columns[c]);
    }
    if !t.query.is_empty() {
        title += &format!(" · /{}", t.query);
    }
    for (c, v) in &t.filters {
        title += &format!(" · {}:{v}", columns[*c]);
    }
    if !t.hidden.is_empty() {
        title += &format!(" · {} hidden (+)", t.hidden.len());
    }
    let shown = &vis[t.left..right];
    let header = Row::new(shown.iter().map(|&c| Cell::from(columns[c].clone()))).style(Style::new().fg(Color::Cyan).bold());
    let body = rows.iter().map(|r| Row::new(shown.iter().map(|&c| Cell::from(r[c].lines().next().unwrap_or_default().to_string()))));
    let widget = Table::new(body, shown.iter().map(|&c| Constraint::Length(widths[c])))
        .header(header)
        .block(block(title, focused))
        .row_highlight_style(Style::new().add_modifier(Modifier::BOLD))
        .cell_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(t.row)).with_selected_column(Some(at - t.left));
    f.render_stateful_widget(widget, area, &mut state);
    if t.open
        && let Some(r) = rows.get(t.row)
    {
        let cell = popup(area, area.width.saturating_sub(4), area.height.saturating_sub(4));
        f.render_widget(Clear, cell);
        let text = if r[t.col].trim().is_empty() { "—".to_string() } else { r[t.col].clone() };
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(block(format!("{} · {}", columns[t.col], r[0]), true)), cell);
    }
    // The rows start under the header.
    let rows_area = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
    clickable(app, Pane::Right, rows_area, Some(state.offset()));
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
    } else if app.right == RightView::Clusters && app.cluster.is_some() && !detail.is_empty() {
        draw_clusters(f, app, detail);
    } else if app.right == RightView::Annotations && !detail.is_empty() {
        draw_annotations(f, app, detail);
    } else if app.right == RightView::Tables && app.table.is_some() {
        draw_table(f, app, detail);
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
        Mode::Prompt(Prompt::TableSearch) => format!("table /{}", app.input),
        Mode::Prompt(Prompt::TableFilter) => {
            let column = app.table.as_ref().and_then(|t| table::columns(&app.lib, &t.name).into_iter().nth(t.col)).map(|c| c.name).unwrap_or_default();
            format!("{column} (!x: not x, \"\": empty): {}", app.input)
        }
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
    match &app.completion {
        Some(c) if app.mode == Mode::Prompt(Prompt::Command) => {
            let mut spans = vec![Span::raw(status), Span::raw("   ")];
            // A few earlier candidates stay in view for context.
            for (i, candidate) in c.candidates.iter().enumerate().skip(c.at.saturating_sub(3)) {
                let span = Span::raw(candidate.clone());
                spans.push(if i == c.at { span.reversed() } else { span.dim() });
                spans.push(Span::raw(" "));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), bar);
        }
        _ => f.render_widget(Paragraph::new(status), bar),
    }
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
        Mode::Setup => draw_setup(f, app, main),
        Mode::Found => draw_found(f, app, main),
        Mode::Sort => draw_sort(f, app, main),
        Mode::TablePick => draw_table_pick(f, app, main),
        Mode::Secrets => draw_secrets(f, app, main),
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
        lines.push(Line::raw(format!("{:>10}  Ctrl-N/Ctrl-P pick · Tab completes", "")).dim());
    }
    for (i, s) in suggestions.iter().enumerate() {
        let line = Line::raw(format!("{:>10}  {s}", ""));
        lines.push(if i == form.pick { line.reversed() } else { line.dim() });
    }
    let height = (FILTER_FIELDS.len() + 2 + FilterForm::SHOWN) as u16 + 2;
    let area = popup(main, 60, height);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block(" Filter · ^D/^U move · ^L clear · ^X all · Enter · Esc ".into(), true)), area);
}

fn draw_setup(f: &mut Frame, app: &App, main: Rect) {
    let Some(form) = &app.setup else { return };
    let fields = form.fields();
    let mut lines: Vec<Line> = fields
        .iter()
        .zip(&form.values)
        .enumerate()
        .map(|(i, ((label, _), value))| {
            let value = if form.masked(i) { "•".repeat(value.chars().count()) } else { value.clone() };
            let cursor = if i == form.sel { "▏" } else { "" };
            let line = Line::from(vec![Span::raw(format!("{label:>10}  ")).bold(), Span::raw(format!("{value}{cursor}"))]);
            if i == form.sel { line.reversed() } else { line }
        })
        .collect();
    lines.push(Line::raw(""));
    let hint = if form.testing { "testing the connection…" } else { fields[form.sel].1 };
    lines.push(Line::raw(format!("{:>10}  {hint}", "")).dim());
    let what = match form.kind {
        crate::setup::Kind::Llm => "LLM profile",
        crate::setup::Kind::Webdav => "WebDAV location",
        crate::setup::Kind::Search => "Saved search",
    };
    let area = popup(main, 76, fields.len() as u16 + 4);
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block(format!(" {what} · Tab/↓ next · Enter {} · Esc cancel ", if form.kind == crate::setup::Kind::Search { "save" } else { "test and save" }), true)), area);
}

/// ✓ found in OpenAlex, ? not found (it may not exist), ◎ already in the library.
fn draw_found(f: &mut Frame, app: &App, main: Rect) {
    let area = popup(main, main.width.saturating_sub(4).min(120), main.height.saturating_sub(2));
    let width = area.width.saturating_sub(10) as usize;
    let items: Vec<ListItem> = app
        .found
        .iter()
        .map(|c| {
            let mark = if app.found_in_library(c) { "◎" } else if c.work.is_some() { "✓" } else { "?" };
            let year = c.found.year.or(c.work.as_ref().and_then(|w| w.year)).map(|y| y.to_string()).unwrap_or_default();
            let head = Line::raw(format!("{mark} {year:4}  {:<16.16} {}", c.found.authors, c.title()));
            let why: String = c.found.why.chars().take(width).collect();
            ListItem::new(vec![head, Line::raw(format!("        {why}")).dim()])
        })
        .collect();
    f.render_widget(Clear, area);
    f.render_stateful_widget(
        List::new(items)
            .block(block(" Found · ✓ in OpenAlex · ? not found · ◎ in library · a add · o open · q close ".into(), true))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut ListState::default().with_selected(Some(app.found_sel)),
    );
}

fn draw_secrets(f: &mut Frame, app: &App, main: Rect) {
    let width = app.secret_items.iter().map(|s| s.label.chars().count()).max().unwrap_or(0);
    let items: Vec<ListItem> = app
        .secret_items
        .iter()
        .map(|s| {
            let style = if s.status == "not set" { Style::new().dim() } else { Style::new().fg(Color::Green) };
            ListItem::new(Line::from(vec![Span::raw(format!("{:<width$}  ", s.label)), Span::styled(s.status.clone(), style)]))
        })
        .collect();
    let lines = items.iter().map(ListItem::width).max().unwrap_or(0) as u16;
    let area = popup(main, lines.max(48) + 2, items.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_stateful_widget(
        List::new(items)
            .block(block(" Secrets · Enter set · d remove · Esc close ".into(), true))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut ListState::default().with_selected(Some(app.secret_sel)),
    );
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

fn draw_table_pick(f: &mut Frame, app: &App, main: Rect) {
    let width = main.width.saturating_sub(4).min(80);
    let items: Vec<ListItem> = app
        .lib
        .tables
        .iter()
        .map(|(name, t)| ListItem::new(vec![Line::from(name.clone()).bold(), Line::from(format!("  {}", t.columns.join(", "))).dim()]))
        .collect();
    let area = popup(main, width, items.len() as u16 * 2 + 2);
    f.render_widget(Clear, area);
    f.render_stateful_widget(
        List::new(items).block(block(" Table · Enter picks ".into(), true)).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut ListState::default().with_selected(Some(app.table_sel)),
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
        HelpPage::Keys => (" Keys · j/k scroll · / search · q close · :help for the guide ", key_help(&app.keymap)),
        HelpPage::Guide => {
            // Headings are the unindented lines.
            let lines = GUIDE
                .lines()
                .map(|l| if l.starts_with(' ') || l.is_empty() { Line::from(l) } else { Line::from(l).bold().fg(Color::Cyan) })
                .collect();
            (" Guide · j/k Ctrl-d/u scroll · / search · q close ", lines)
        }
        HelpPage::Failed => (if app.report_copied { " Failed · copied to the clipboard · q close " } else { " Failed · q close " }, app.report.iter().map(|l| Line::from(l.as_str())).collect()),
    };
    app.help_lines = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
    let lines: Vec<Line> = lines.into_iter().map(|l| highlight(l, &app.help_query)).collect();
    let area = main;
    app.help_scroll = app.help_scroll.min((lines.len() as u16).saturating_sub(area.height.saturating_sub(2)));
    let mut title = title.to_string();
    if app.help_typing || !app.help_query.is_empty() {
        title = format!(" /{}{} · n/N next/previous · Esc clears ", app.help_query, if app.help_typing { "▏" } else { "" });
    }
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).scroll((app.help_scroll, 0)).block(block(title, true)), area);
}

/// `line` with each case-insensitive occurrence of `query` highlighted.
fn highlight<'a>(line: Line<'a>, query: &str) -> Line<'a> {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let lower = text.to_lowercase();
    let query = query.to_lowercase();
    // Lower-casing can change byte lengths outside ASCII; such lines go unhighlighted.
    if query.is_empty() || lower.len() != text.len() || !lower.contains(&query) {
        return line;
    }
    let mut spans = Vec::new();
    let mut at = 0;
    for (i, _) in lower.match_indices(&query) {
        if i < at {
            continue;
        }
        spans.push(Span::raw(text[at..i].to_string()));
        spans.push(Span::raw(text[i..i + query.len()].to_string()).black().on_yellow());
        at = i + query.len();
    }
    spans.push(Span::raw(text[at..].to_string()));
    Line::from(spans).style(line.style)
}

/// Rows by year, newest at the top; ◉ seeds, ● other library papers, ○ papers outside it. The
/// selected paper is yellow, its neighbours cyan, and the neighbour highlighted in the list a magenta ◆.
/// Unused for edges away from the selection, which would fill a large graph; the selected
/// paper's darker, the picked neighbour's brightest.
const EDGE_COLORS: [Color; 3] = [Color::DarkGray, Color::Yellow, Color::LightYellow];
/// The selected paper's edges in the citation graph, normal and picked: to papers it cites, and from papers citing it.
const CITES_COLORS: [Color; 2] = [Color::Yellow, Color::LightYellow];
const CITED_BY_COLORS: [Color; 2] = [Color::Magenta, Color::LightMagenta];

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

/// Starting seeds ⊚, seeds added while exploring ⊙, other library papers ◎, outside papers ○.
fn marker(n: &yeda_core::graph::Node) -> &'static str {
    if n.explored {
        "⊙"
    } else if n.seed {
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

/// Colours for labels without a tag colour of their own.
const LABEL_COLORS: [Color; 6] = [Color::Green, Color::Blue, Color::Magenta, Color::Red, Color::LightCyan, Color::LightGreen];

fn draw_clusters(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.pane == Pane::Right;
    let Some(v) = &app.cluster else { return };
    let nodes = &v.clusters.nodes;
    let near = v.entries();
    let list_height = (near.len() as u16 + 2).clamp(5, area.height / 2);
    let [top, bottom] = Layout::vertical([Constraint::Min(6), Constraint::Length(list_height)]).areas(area);
    let picked = near.get(v.nbr).copied().filter(|_| focused);
    let is_label = |i: usize| nodes[i].kind == Kind::Label;
    let rank = |i: usize| match i {
        _ if i == v.sel => 4,
        _ if Some(i) == picked => 3,
        _ if nodes[v.sel].links.contains(&i) => 2,
        _ if is_label(i) => 1,
        _ => 0,
    };
    let label_color = |i: usize| {
        let colors = nodes.iter().take(i).filter(|n| n.kind == Kind::Label).count();
        let tag = (v.what == "tags").then(|| app.lib.tag_colors.get(&nodes[i].name).and_then(|c| color(c))).flatten();
        tag.unwrap_or(LABEL_COLORS[colors % LABEL_COLORS.len()])
    };
    let text = |i: usize| {
        let n = &nodes[i];
        if !is_label(i) {
            return n.name.clone();
        }
        let name: String = n.name.chars().take(28).collect();
        let cut = if name.len() < n.name.len() { "…" } else { "" };
        format!("{name}{cut} ({})", n.links.len())
    };
    // Text cells already taken, so labels never overlap: the most important nodes claim theirs first.
    let (w, h) = (top.width.saturating_sub(2).max(1) as i32, top.height.saturating_sub(2).max(1) as i32);
    let pos = |i: usize| (nodes[i].x * 0.96 + 0.02, nodes[i].y * 0.9 + 0.05);
    // The cell the canvas prints a point in: it truncates, it doesn't round.
    let cell = |i: usize| {
        let (x, y) = pos(i);
        ((x * (w - 1) as f64) as i32, ((1.0 - y) * (h - 1) as f64) as i32)
    };
    // A point inside cell (c, r), for printing there.
    let at = |c: i32, r: i32| (((c as f64 + 0.01) / (w - 1).max(1) as f64).min(1.0), (1.0 - (r as f64 + 0.01) / (h - 1).max(1) as f64).max(0.0));
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(rank(i)));
    let mut taken: Vec<(i32, i32, i32)> = Vec::new();
    let mut labelled = vec![None; nodes.len()];
    for &i in &order {
        let (c, r) = cell(i);
        let len = text(i).chars().count() as i32;
        let free = |(a, b): (i32, i32)| a >= -1 && b <= w + 1 && !taken.iter().any(|&(row, x, y)| row == r && a < y && x < b);
        let side = [(Label::Right, (c - 1, c + 3 + len)), (Label::Left, (c - 2 - len, c + 2))].into_iter().find(|&(_, span)| free(span));
        match side.filter(|_| rank(i) > 0 || is_label(i)) {
            Some((label, (a, b))) => {
                taken.push((r, a, b));
                labelled[i] = Some(label);
            }
            None => taken.push((r, c - 1, c + 2)),
        }
    }
    let canvas = Canvas::default()
        .block(block(format!("{} of {} papers · {} values", v.what, nodes.iter().filter(|n| !matches!(n.kind, Kind::Label)).count(), nodes.iter().filter(|n| n.kind == Kind::Label).count()), focused))
        .marker(Marker::Braille)
        .x_bounds([0.0, 1.0])
        .y_bounds([0.0, 1.0])
        .paint(|ctx| {
            for (level, &color) in EDGE_COLORS.iter().enumerate().skip(1) {
                for &j in &nodes[v.sel].links {
                    if (level == 2) == (Some(j) == picked) {
                        let ((x1, y1), (x2, y2)) = (pos(v.sel), pos(j));
                        ctx.draw(&CanvasLine { x1, y1, x2, y2, color });
                    }
                }
                ctx.layer();
            }
            for &i in order.iter().rev() {
                let style = match rank(i) {
                    4 => Style::new().fg(Color::Yellow),
                    3 => Style::new().fg(Color::LightYellow).bold(),
                    2 => Style::new().fg(Color::Cyan),
                    1 => Style::new().fg(label_color(i)),
                    _ => Style::new().dim(),
                };
                let glyph = match (rank(i), is_label(i)) {
                    (3, _) => "◆",
                    (_, true) => "■",
                    _ => "•",
                };
                let (c, r) = cell(i);
                let (x, y) = at(c, r);
                ctx.print(x, y, Span::styled(glyph, style));
                let start = match labelled[i] {
                    Some(Label::Right) => c + 2,
                    Some(Label::Left) => c - 1 - text(i).chars().count() as i32,
                    _ => continue,
                };
                let (x, y) = at(start, r);
                ctx.print(x, y, Span::styled(text(i), style));
            }
        });
    f.render_widget(canvas, top);

    let row = |i: usize| {
        let n = &nodes[i];
        match n.kind {
            Kind::Label => Line::from(vec![Span::styled("■ ", Style::new().fg(label_color(i))), Span::raw(format!("{} ({})", n.name, n.links.len()))]),
            Kind::Paper(_) => Line::from(format!("• {:<16.16} {}", n.name, n.title)),
        }
    };
    let items: Vec<ListItem> = near.iter().map(|&i| ListItem::new(row(i))).collect();
    let mut title: Vec<Span> = Vec::new();
    for (all, name) in [(false, "Linked"), (true, "All")] {
        let span = Span::raw(format!(" {name} "));
        title.push(if all == v.all { span.reversed() } else { span.dim() });
    }
    title.push(Span::raw(format!(" · Tab · {} ", nodes[v.sel].name)));
    let mut state = ListState::default().with_selected(focused.then_some(v.nbr));
    let border = if focused { Style::new().fg(Color::Cyan) } else { Style::new() };
    let list = List::new(items).block(Block::bordered().title(Line::from(title)).border_style(border)).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, bottom, &mut state);
    clickable(app, Pane::Right, bottom, Some(state.offset()));
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
            for level in 1..=2 {
                for &e in edges.iter().filter(|&&e| edge_level(e) == level) {
                    let colors = if e.0 == v.sel { CITES_COLORS } else { CITED_BY_COLORS };
                    ctx.draw(&edge(e, colors[level - 1]));
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
        let span = match tab {
            GraphTab::Cites => span.fg(CITES_COLORS[0]),
            GraphTab::CitedBy => span.fg(CITED_BY_COLORS[0]),
            _ => span,
        };
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
