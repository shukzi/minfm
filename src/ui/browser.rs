use super::dialogs::viewport_start;
use super::*;

pub(super) fn draw_browser(frame: &mut Frame, app: &App, area: Rect) {
    match app.browser_view {
        BrowserView::Tree => draw_tree_view(frame, app, area),
        BrowserView::Table => draw_table_view(frame, app, area),
    }
}

pub(super) fn draw_table_view(frame: &mut Frame, app: &App, area: Rect) {
    if area.width < 86 {
        draw_file_table(frame, app, area);
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(area);
    draw_file_table(frame, app, columns[0]);
    draw_details(frame, app, columns[1]);
}

pub(super) fn draw_tree_view(frame: &mut Frame, app: &App, area: Rect) {
    let icons = Icons::new(&app.config.icons);
    let search_query: Option<&str> = None;
    let visible_count = usize::from(area.height.saturating_sub(1)).max(1);
    let start = viewport_start(app.cursor, app.entries.len(), visible_count);
    let end = (start + visible_count).min(app.entries.len());
    let rows = app.entries[start..end]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let index = start + offset;
            let depth = app.tree_depth(index);
            let marker = if entry.selected { "●" } else { " " };
            let expanded =
                entry.kind == EntryKind::Directory && app.is_tree_directory_expanded(&entry.path);
            let name = Line::from(vec![
                Span::styled(
                    tree_line_prefix(app, index, depth),
                    Style::default().fg(MUTED),
                ),
                icon_span(Icons::slot(icons.entry(entry, expanded))),
                Span::raw(" "),
                Span::raw(entry.name.clone()),
            ]);
            let name_cell = if search_query.is_some() {
                Cell::from(name.style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
            } else {
                Cell::from(name)
            };
            Row::new(vec![
                Cell::from(marker),
                name_cell,
                Cell::from(entry.size_text()),
                Cell::from(entry.permissions()),
                Cell::from(entry.modified_text()),
            ])
        });
    let size_width = if app.config.ui.show_size { 10 } else { 0 };
    let permission_width = if app.config.ui.show_permissions {
        11
    } else {
        0
    };
    let modified_width = if app.config.ui.show_modified { 19 } else { 0 };
    let widths = if area.width > 105 {
        vec![
            Constraint::Length(2),
            Constraint::Min(18),
            Constraint::Length(size_width),
            Constraint::Length(permission_width),
            Constraint::Length(modified_width),
        ]
    } else if area.width > 65 {
        vec![
            Constraint::Length(2),
            Constraint::Min(18),
            Constraint::Length(size_width),
            Constraint::Length(permission_width),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Min(15),
            Constraint::Length(10),
            Constraint::Length(0),
            Constraint::Length(0),
        ]
    };
    let table = Table::new(rows, widths)
        .header(
            Row::new(["", "Tree", "Size", "Permissions", "Modified"])
                .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("> ");
    let selected = (!app.entries.is_empty()).then_some(app.cursor.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

pub(super) fn tree_line_prefix(app: &App, index: usize, depth: usize) -> String {
    let mut prefix = String::with_capacity(depth.saturating_mul(4) + 4);
    for ancestor_depth in 0..depth {
        if has_later_tree_sibling(app, index, ancestor_depth) {
            prefix.push_str("│   ");
        } else {
            prefix.push_str("    ");
        }
    }
    if has_later_tree_sibling(app, index, depth) {
        prefix.push_str("├── ");
    } else {
        prefix.push_str("└── ");
    }
    prefix
}

pub(super) fn has_later_tree_sibling(app: &App, index: usize, depth: usize) -> bool {
    for candidate in index.saturating_add(1)..app.entries.len() {
        let candidate_depth = app.tree_depth(candidate);
        if candidate_depth < depth {
            return false;
        }
        if candidate_depth == depth {
            return true;
        }
    }
    false
}

pub(super) fn draw_file_table(frame: &mut Frame, app: &App, area: Rect) {
    let icons = Icons::new(&app.config.icons);
    let search_query: Option<&str> = None;
    let visible_count = usize::from(area.height.saturating_sub(3)).max(1);
    let start = viewport_start(app.cursor, app.entries.len(), visible_count);
    let end = (start + visible_count).min(app.entries.len());
    let rows = app.entries[start..end].iter().map(|entry| {
        let marker = if entry.selected { "●" } else { " " };
        let name = Line::from(vec![
            icon_span(Icons::slot(icons.entry(entry, false))),
            Span::raw(" "),
            Span::raw(entry.name.clone()),
        ]);
        let name_cell = if search_query.is_some() {
            Cell::from(name.style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
        } else {
            Cell::from(name)
        };
        Row::new(vec![
            Cell::from(marker),
            name_cell,
            Cell::from(entry.size_text()),
            Cell::from(entry.permissions()),
            Cell::from(entry.modified_text()),
        ])
    });
    let size_width = if app.config.ui.show_size { 10 } else { 0 };
    let permission_width = if app.config.ui.show_permissions {
        11
    } else {
        0
    };
    let modified_width = if app.config.ui.show_modified { 19 } else { 0 };
    let widths = if area.width > 105 {
        vec![
            Constraint::Length(2),
            Constraint::Min(18),
            Constraint::Length(size_width),
            Constraint::Length(permission_width),
            Constraint::Length(modified_width),
        ]
    } else if area.width > 65 {
        vec![
            Constraint::Length(2),
            Constraint::Min(18),
            Constraint::Length(size_width),
            Constraint::Length(permission_width),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Min(15),
            Constraint::Length(10),
            Constraint::Length(0),
            Constraint::Length(0),
        ]
    };
    let table = Table::new(rows, widths)
        .header(
            Row::new(["", "Name", "Size", "Permissions", "Modified"])
                .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(" Files "));
    let selected = (!app.entries.is_empty()).then_some(app.cursor.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

pub(super) fn icon_span(icon: String) -> Span<'static> {
    Span::styled(
        icon,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
}

pub(super) fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let text = if let Some(entry) = app.selected_entry() {
        let clipboard = app
            .clipboard
            .as_ref()
            .map(|clip| {
                format!(
                    "{} item(s) {}",
                    clip.paths.len(),
                    match clip.mode {
                        ClipboardMode::Copy => "copied",
                        ClipboardMode::Cut => "cut",
                    }
                )
            })
            .unwrap_or_else(|| "empty".into());
        format!(
            "Name: {}\nType: {:?}\nSize: {}\nPermissions: {}\nModified: {}\n\nClipboard: {}",
            entry.name,
            entry.kind,
            entry.size_text(),
            entry.permissions(),
            entry.modified_text(),
            clipboard,
        )
    } else {
        "This directory is empty.".into()
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        area,
    );
}
