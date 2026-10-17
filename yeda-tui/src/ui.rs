use crate::app::{App, FilterForm, GUIDE, HELP, HelpPage, Mode, Pane, PickKind, Prompt, RightView};
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

fn block(title: String, focused: bool) -> Block<'static> {
    let style = if focused { Style::new().fg(Color::Cyan) } else { Style::new() };
    Block::bordered().title(title).border_style(style)
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
    f.render_stateful_widget(
        List::new(items)
            .block(block(title, focused))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        top,
        &mut ListState::default().with_selected(Some(app.annot_sel)),
    );

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
    let hl = Style::new().add_modifier(Modifier::REVERSED);

    let items: Vec<ListItem> = app.side_visible.iter().map(|&i| ListItem::new(label(&app.sidebar[i], &app.lib))).collect();
    let title = if app.side_query.is_empty() { "Library".into() } else { format!("Library /{}", app.side_query) };
    f.render_stateful_widget(
        List::new(items).block(block(title, app.pane == Pane::Sidebar)).highlight_style(hl),
        side,
        &mut ListState::default().with_selected(Some(app.side_sel)),
    );

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
    f.render_stateful_widget(
        List::new(items).block(block(title, app.pane == Pane::List)).highlight_style(hl),
        list,
        &mut ListState::default().with_selected(Some(app.sel)),
    );

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

fn draw_help(f: &mut Frame, app: &mut App, main: Rect, page: HelpPage) {
    let (title, lines): (&str, Vec<Line>) = match page {
        HelpPage::Keys => {
            let width = HELP.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
            let lines = HELP
                .iter()
                .map(|(k, v)| Line::from(vec![Span::styled(format!("{k:<width$}  "), Style::new().fg(Color::Cyan)), Span::raw(*v)]))
                .collect();
            (" Keys · j/k scroll · q close · :help for the guide ", lines)
        }
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
fn draw_graph(f: &mut Frame, app: &App, area: Rect) {
    let Some(v) = &app.graph else { return };
    if area.height < 10 {
        return;
    }
    let g = &v.graph;
    let focused = app.pane == Pane::Right;
    let near = v.entries();
    let list_height = (near.len() as u16 + 3).clamp(5, area.height / 2);
    let [top, bottom] = Layout::vertical([Constraint::Min(6), Constraint::Length(list_height)]).areas(area);
    let rows = g.years.len().max(1) as f64;
    let pos = |i: usize| (g.nodes[i].x, rows - g.nodes[i].row as f64 - 0.5);
    let picked = near.get(v.nbr).map(|&(_, j)| j).filter(|_| focused);
    // Two character cells, in canvas units, between a node and its label.
    let gap = 2.0 * 1.15 / top.width.saturating_sub(2).max(1) as f64;
    let edge = |(a, b): (usize, usize), color| {
        let ((x1, y1), (x2, y2)) = (pos(a), pos(b));
        CanvasLine { x1, y1, x2, y2, color }
    };
    let canvas = Canvas::default()
        .block(block(format!("Graph of {} ({} papers)", v.scope, g.nodes.len()), focused))
        .marker(Marker::Braille)
        .x_bounds([-0.15, 1.0])
        .y_bounds([0.0, rows])
        .paint(|ctx| {
            // ponytail: past this many edges braille lines merge into a solid block, so only the selection's are drawn.
            if g.edges.len() <= 60 {
                for &e in g.edges.iter().filter(|e| e.0 != v.sel && e.1 != v.sel) {
                    ctx.draw(&edge(e, Color::DarkGray));
                }
            }
            ctx.layer();
            for &e in g.edges.iter().filter(|e| e.0 == v.sel || e.1 == v.sel) {
                ctx.draw(&edge(e, Color::Yellow));
            }
            if let Some(p) = picked {
                ctx.layer();
                ctx.draw(&edge((v.sel, p), Color::Magenta));
            }
            ctx.layer();
            // Label only as many year rows as fit.
            let step = (rows / top.height.saturating_sub(2).max(1) as f64).ceil().max(1.0) as usize;
            for (row, year) in g.years.iter().enumerate().step_by(step) {
                let label = year.map_or("?".to_string(), |y| y.to_string());
                ctx.print(-0.15, rows - row as f64 - 0.5, Span::raw(label).dim());
            }
            // Later prints cover earlier ones, so the papers that matter most go last.
            let rank = |i: usize| match i {
                _ if i == v.sel => 3,
                _ if Some(i) == picked => 2,
                _ if near.iter().any(|&(_, j)| j == i) => 1,
                _ => 0,
            };
            let mut order: Vec<usize> = (0..g.nodes.len()).collect();
            order.sort_by_key(|&i| rank(i));
            for i in order {
                let n = &g.nodes[i];
                let (glyph, style) = match rank(i) {
                    3 => (None, Style::new().fg(Color::Yellow).bold()),
                    2 => (Some("◆"), Style::new().fg(Color::Magenta).bold()),
                    1 => (None, Style::new().fg(Color::Cyan)),
                    _ => (None, Style::new().dim()),
                };
                let glyph = glyph.unwrap_or(if n.seed { "◉" } else if n.key.is_some() { "●" } else { "○" });
                let label = format!("{} {}", n.first_author, n.year.map(|y| y.to_string()).unwrap_or_default());
                let (x, y) = pos(i);
                ctx.print(x, y, Span::styled(glyph, style));
                ctx.print(x + gap, y, Span::styled(label, style));
            }
        });
    f.render_widget(canvas, top);

    let node = &g.nodes[v.sel];
    let label = |n: &yeda_core::graph::Node| {
        let year = n.year.map(|y| y.to_string()).unwrap_or_default();
        let mark = if n.key.is_some() { "●" } else { "○" };
        format!("{mark} {year:4} {:<12.12} {}", n.first_author, n.title)
    };
    let items: Vec<ListItem> = near
        .iter()
        .map(|(mark, i)| ListItem::new(format!("{mark:<3} {}", label(&g.nodes[*i]))))
        .collect();
    let title = if v.lists {
        "▲ cited by the most seeds · ▼ citing the most seeds · L: neighbours".to_string()
    } else {
        format!("{} · ← cites it, → it cites · L: top lists", label(node))
    };
    let mut state = ListState::default().with_selected(focused.then_some(v.nbr));
    let list = List::new(items).block(block(title, focused)).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, bottom, &mut state);
}
