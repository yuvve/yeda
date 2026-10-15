use crate::app::{App, Mode, Pane, PickKind, Prompt, RightView};
use yeda_core::annotations::Annotation;
use yeda_core::browse::Filter;
use yeda_core::library::{Library, Ref};
use ratatui::prelude::*;
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

fn row(r: &Ref, lib: &Library) -> Line<'static> {
    let year = r.year.map(|y| y.to_string()).unwrap_or_default();
    let author = r.authors.first().and_then(|a| a.split(',').next()).unwrap_or("");
    let mut spans = vec![Span::raw(format!("{} {year:4}  {author:<16.16} ", status_glyph(r.status.as_deref())))];
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

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, bar] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let [side, list, detail] = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(40),
        Constraint::Percentage(40),
    ])
    .areas(main);
    app.page = list.height.saturating_sub(2) as usize;
    let hl = Style::new().add_modifier(Modifier::REVERSED);

    let items: Vec<ListItem> = app.side_visible.iter().map(|&i| ListItem::new(label(&app.sidebar[i], &app.lib))).collect();
    let title = if app.side_query.is_empty() { "Library".into() } else { format!("Library /{}", app.side_query) };
    f.render_stateful_widget(
        List::new(items).block(block(title, app.pane == Pane::Sidebar)).highlight_style(hl),
        side,
        &mut ListState::default().with_selected(Some(app.side_sel)),
    );

    let items: Vec<ListItem> = app.visible.iter().map(|&i| ListItem::new(row(&app.lib.refs[i], &app.lib))).collect();
    let title = format!("References ({})", app.visible.len());
    f.render_stateful_widget(
        List::new(items).block(block(title, app.pane == Pane::List)).highlight_style(hl),
        list,
        &mut ListState::default().with_selected(Some(app.sel)),
    );

    if app.right == RightView::Annotations {
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
        Mode::Prompt(Prompt::Add) => format!("add DOI / arXiv / ISBN: {}", app.input),
        _ if !app.status.is_empty() => app.status.clone(),
        _ if !app.query.is_empty() => format!("/{}", app.query),
        _ => String::new(),
    };
    f.render_widget(Paragraph::new(status), bar);
    if let Some(c) = &app.chooser {
        let height = (c.options.len().min(10) as u16 + 2).min(bar.y);
        let area = Rect { x: list.x, y: bar.y - height, width: list.width, height };
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
}
