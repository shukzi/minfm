use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap,
    },
    Frame,
};

use crate::{
    app::{
        format_elapsed, App, AppMode, AppsView, ArchiveView, BrowserView, BuiltinApp,
        ClipboardMode, DeviceView, NetworkView, PartitionOverlay, PartitionView, Prompt,
        SearchForm, SearchView, TrashView,
    },
    entry::{human_size, EntryKind},
    icons::Icons,
    partition::Filesystem,
};
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Gray;
const MUTED: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(shortcut_bar_height(app, frame.area().width)),
        ])
        .split(frame.area());

    draw_header(frame, app, rows[0]);
    draw_browser(frame, app, rows[1]);

    match &app.mode {
        AppMode::Browser => {}
        AppMode::Archive(view) => draw_archive(frame, app, view),
        AppMode::Apps(view) => draw_apps(frame, app, view),
        AppMode::Prompt(prompt) => draw_prompt(frame, app, prompt),
        AppMode::Progress => draw_progress_modal(frame, app),
        AppMode::SearchProgress => draw_search_progress(frame, app),
        AppMode::SearchForm(form) => draw_search_form(frame, app, form),
        AppMode::SearchResults => {
            if let Some(view) = &app.search_results {
                draw_search_results(frame, app, view);
            }
        }
        AppMode::UpdateProgress => draw_update_progress(frame),
        AppMode::Trash(view) => draw_trash(frame, app, view),
        AppMode::Devices(view) => draw_devices(frame, app, view),
        AppMode::Network(view) => draw_network(frame, app, view),
        AppMode::NetworkProgress => draw_network_progress(frame),
        AppMode::Partitions(view) => draw_partitions(frame, app, view),
        AppMode::Help => draw_help(frame, app),
        AppMode::Info => draw_info(frame, app),
        AppMode::ConfigError { path, error } => draw_config_error(frame, app, path, error),
    }

    // Keep these bars above app panels so the active context's shortcuts are
    // always visible, including when a panel uses most of the terminal.
    draw_status(frame, app, rows[2]);
    draw_shortcuts(frame, app, rows[3]);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(" {}", app.current_dir.display());
    let mode = if app.config.behavior.read_only {
        "READ ONLY"
    } else {
        "minfm"
    };
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" minfm "),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let arrow = if app.config.ui.reverse_sort {
        "↓"
    } else {
        "↑"
    };
    let full_actions = format!(
        "{} Trash   {} Info   {} Devices   Sort: {} {}",
        app.config.hotkeys.trash_bin.display(),
        app.config.hotkeys.info.display(),
        app.config.hotkeys.devices.display(),
        app.sort_label(),
        arrow,
    );
    let compact = usize::from(inner.width) < UnicodeWidthStr::width(full_actions.as_str()) + 18;
    let action_width = if compact {
        UnicodeWidthStr::width(
            format!(
                "{}  {}  {}  Sort {}",
                app.config.hotkeys.trash_bin.display(),
                app.config.hotkeys.info.display(),
                app.config.hotkeys.devices.display(),
                arrow
            )
            .as_str(),
        )
    } else {
        UnicodeWidthStr::width(full_actions.as_str())
    };
    let action_width = u16::try_from(action_width).unwrap_or(u16::MAX);
    let columns = Layout::horizontal([
        Constraint::Min(4),
        Constraint::Length(action_width.min(inner.width.saturating_sub(4))),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(mode, Style::default().fg(MUTED)),
        ])),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(header_action_line(app, arrow, compact)).alignment(Alignment::Right),
        columns[1],
    );
}

fn header_action_line(app: &App, arrow: &str, compact: bool) -> Line<'static> {
    let key_style = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(MUTED);
    let mut spans = Vec::new();
    for (key, label) in [
        (app.config.hotkeys.trash_bin.display(), "Trash"),
        (app.config.hotkeys.info.display(), "Info"),
        (app.config.hotkeys.devices.display(), "Devices"),
    ] {
        if !spans.is_empty() {
            spans.push(Span::raw(if compact { "  " } else { "   " }));
        }
        spans.push(Span::styled(key.to_owned(), key_style));
        if !compact {
            spans.push(Span::styled(format!(" {label}"), label_style));
        }
    }
    spans.push(Span::raw(if compact { "  " } else { "   " }));
    spans.push(Span::styled(
        if compact {
            format!("Sort {arrow}")
        } else {
            format!("Sort: {} {arrow}", app.sort_label())
        },
        label_style,
    ));
    Line::from(spans)
}

fn draw_browser(frame: &mut Frame, app: &App, area: Rect) {
    match app.browser_view {
        BrowserView::Tree => draw_tree_view(frame, app, area),
        BrowserView::Table => draw_table_view(frame, app, area),
    }
}

fn draw_table_view(frame: &mut Frame, app: &App, area: Rect) {
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

fn draw_tree_view(frame: &mut Frame, app: &App, area: Rect) {
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

fn tree_line_prefix(app: &App, index: usize, depth: usize) -> String {
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

fn has_later_tree_sibling(app: &App, index: usize, depth: usize) -> bool {
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

fn draw_file_table(frame: &mut Frame, app: &App, area: Rect) {
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

fn icon_span(icon: String) -> Span<'static> {
    Span::styled(
        icon,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
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

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    if let AppMode::Archive(view) = &app.mode {
        let noun = if view.entries.len() == 1 {
            "archive item"
        } else {
            "archive items"
        };
        frame.render_widget(
            Paragraph::new(format!(" {} {noun}", view.entries.len()))
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    let selected = app.entries.iter().filter(|entry| entry.selected).count();
    let arrow = if app.config.ui.reverse_sort {
        "↓"
    } else {
        "↑"
    };
    let loading = if app.browser_loading {
        format!(" · loading: {}", app.browser_loaded_entries)
    } else {
        String::new()
    };
    let base = format!(
        " {} items · selected: {} · hidden: {} · sort: {} {}{}{}",
        app.entries.len(),
        selected,
        if app.config.ui.show_hidden {
            "on"
        } else {
            "off"
        },
        app.sort_label(),
        arrow,
        "",
        loading,
    );
    let message = app.visible_status();
    let status = if message.is_empty() {
        base
    } else {
        format!("{base} · {message}")
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(MUTED)),
        area,
    );
}

fn draw_shortcuts(frame: &mut Frame, app: &App, area: Rect) {
    let context = match &app.mode {
        AppMode::Browser => None,
        AppMode::Apps(_) => Some(format!(
            " ↑↓/{}/{} Move · Enter Open · {}/{}/Esc Close ",
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display(),
            app.config.hotkeys.tools.display(),
            app.config.hotkeys.quit.display(),
        )),
        AppMode::Archive(_) => Some(format!(
            " ↑↓/{}/{} Move · {}/Esc Close ",
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display(),
            app.config.hotkeys.quit.display(),
        )),
        AppMode::Partitions(view) => Some(partition_shortcuts(app, view)),
        AppMode::Prompt(Prompt::PartitionAuthentication { .. }) => {
            Some(" Type administrator password · Enter Authenticate · Esc Cancel ".into())
        }
        _ => Some(" A dialog owns input · File shortcuts are disabled until it closes. ".into()),
    };
    if let Some(context) = context {
        frame.render_widget(
            Paragraph::new(context)
                .alignment(Alignment::Left)
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }

    let items = browser_shortcut_items(app);
    let lines = shortcut_lines_owned(&items, area.width, usize::from(area.height));
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().fg(MUTED)),
        area,
    );
}

fn browser_shortcut_items(app: &App) -> Vec<(String, &'static str)> {
    let mut items = vec![
        (
            format!(
                "↑↓/{}/{}",
                app.config.hotkeys.down.display(),
                app.config.hotkeys.up.display()
            ),
            "Move",
        ),
        (
            format!("←/{}", app.config.hotkeys.collapse.display()),
            "Parent",
        ),
        (format!("→/{}", app.config.hotkeys.expand.display()), "Open"),
        ("Enter".into(), "Open"),
        (app.config.hotkeys.select.display().into(), "Mark"),
        (app.config.hotkeys.cut.display().into(), "Cut"),
        (app.config.hotkeys.copy.display().into(), "Copy"),
        (app.config.hotkeys.paste.display().into(), "Paste"),
        (app.config.hotkeys.archive.display().into(), "Archive"),
        (app.config.hotkeys.rename.display().into(), "Rename"),
        (app.config.hotkeys.create_file.display().into(), "File"),
        (
            app.config.hotkeys.create_directory.display().into(),
            "Directory",
        ),
        (app.config.hotkeys.tools.display().into(), "Tools"),
        (app.config.hotkeys.network_shares.display().into(), "Shares"),
        (app.config.hotkeys.hidden.display().into(), "Hidden"),
        (app.config.hotkeys.go_to.display().into(), "Path"),
        (app.config.hotkeys.search.display().into(), "Search"),
        (app.config.hotkeys.help.display().into(), "Help"),
    ];
    if app
        .selected_entry()
        .is_some_and(|entry| entry.is_text_file())
    {
        items.push((app.config.hotkeys.edit.display().into(), "Edit"));
    }
    items
}

fn shortcut_bar_height(app: &App, width: u16) -> u16 {
    let needs_second_line = matches!(app.mode, AppMode::Browser)
        && shortcut_items_width(&browser_shortcut_items(app)) > usize::from(width);
    if needs_second_line {
        2
    } else {
        1
    }
}

fn shortcut_items_width(items: &[(String, &'static str)]) -> usize {
    1 + items
        .iter()
        .map(|(key, label)| UnicodeWidthStr::width(key.as_str()) + 1 + label.len())
        .sum::<usize>()
        + items.len().saturating_sub(1) * 3
}

fn shortcut_lines_owned(
    items: &[(String, &'static str)],
    width: u16,
    max_lines: usize,
) -> Vec<Line<'static>> {
    let width = usize::from(width).max(1);
    let max_lines = max_lines.max(1);
    let mut lines = vec![vec![Span::raw(" ")]];
    let mut line_width = 1usize;
    for (key, label) in items {
        let pair_width = UnicodeWidthStr::width(key.as_str()) + 1 + label.len();
        let separator = if line_width > 1 { 3 } else { 0 };
        if line_width + separator + pair_width > width && line_width > 1 {
            if lines.len() == max_lines {
                break;
            }
            lines.push(vec![Span::raw(" ")]);
            line_width = 1;
        }
        let separator = if line_width > 1 { 3 } else { 0 };
        if separator > 0 {
            lines.last_mut().unwrap().push(Span::raw("   "));
        }
        lines.last_mut().unwrap().extend([
            Span::styled(
                key.clone(),
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {label}"), Style::default().fg(MUTED)),
        ]);
        line_width += separator + pair_width;
    }
    lines.into_iter().map(Line::from).collect()
}

fn partition_shortcuts(app: &App, view: &crate::app::PartitionView) -> String {
    let down = app.config.hotkeys.down.display();
    let up = app.config.hotkeys.up.display();
    let actions = app.config.hotkeys.partition_actions.display();
    let refresh = app.config.hotkeys.refresh.display();
    let hint = match &view.overlay {
        None => {
            let exit = if app.partition_returns_to_apps() {
                format!(
                    "Esc Back to menu · {} Files",
                    app.config.hotkeys.quit.display()
                )
            } else {
                format!(
                    "Esc/{} Back to files · {} Menu",
                    app.config.hotkeys.quit.display(),
                    app.config.hotkeys.tools.display()
                )
            };
            return format!(
                " ↑↓/{down}/{up} Move · Enter/{actions} Actions · {refresh} Refresh · {exit} "
            );
        }
        Some(crate::app::PartitionOverlay::Actions { .. }) => {
            return format!(" ↑↓/{down}/{up} Move · Enter Continue · {actions}/Esc Back ")
        }
        Some(crate::app::PartitionOverlay::FormatOptions { .. }) => {
            return format!(" ↑↓/{down}/{up} Choose filesystem · Enter Continue · Esc Back ")
        }
        Some(crate::app::PartitionOverlay::EncryptionFilesystem { .. }) => {
            return format!(" ↑↓/{down}/{up} Choose inner filesystem · Enter Continue · Esc Back ")
        }
        Some(crate::app::PartitionOverlay::EncryptionPassphrase { .. })
        | Some(crate::app::PartitionOverlay::ChangePassphrase { .. }) => {
            " Enter Continue · Esc Back "
        }
        Some(crate::app::PartitionOverlay::DiskLayoutOptions { .. }) => {
            return format!(" ↑↓/{down}/{up} Choose layout · Enter Review · Esc Back ")
        }
        Some(crate::app::PartitionOverlay::FreeRegionOptions { .. }) => {
            return format!(" ↑↓/{down}/{up} Choose free space · Enter Continue · Esc Back ")
        }
        Some(crate::app::PartitionOverlay::PartitionSize { .. }) => {
            " Type size or max · Enter Review · Esc Regions "
        }
        Some(crate::app::PartitionOverlay::FormatLabel { .. }) => {
            " Type optional label · Enter Review · Esc Filesystems "
        }
        Some(crate::app::PartitionOverlay::Input { .. }) => " Enter Review · Esc Back ",
        Some(crate::app::PartitionOverlay::Confirm { .. }) => {
            " ←/→ Choose · Enter Apply · Esc Cancel "
        }
    };
    hint.into()
}

fn draw_prompt(frame: &mut Frame, app: &App, prompt: &Prompt) {
    match prompt {
        Prompt::GoTo { input } => input_modal(frame, "Go to path", input, "Enter go · Esc cancel"),
        Prompt::Rename { input, cursor, .. } => cursor_input_modal(
            frame,
            "Rename",
            "Enter a new name:",
            input,
            *cursor,
            "rename",
        ),
        Prompt::CreateDirectory { input } => input_modal(
            frame,
            "Create directory",
            input,
            "Enter create · Esc cancel",
        ),
        Prompt::CreateFile { input, cursor } => cursor_input_modal(
            frame,
            "Create file",
            "Enter a file name:",
            input,
            *cursor,
            "create",
        ),
        Prompt::ArchiveFormat { selected, .. } => {
            let body = crate::archive::ArchiveFormat::ALL
                .iter()
                .enumerate()
                .map(|(index, format)| {
                    format!(
                        "{} {}",
                        if index == *selected { ">" } else { " " },
                        format.label()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            message_modal(
                frame,
                "Create archive",
                &format!("Choose a format:\n\n{body}"),
                "↑/↓ choose · Enter continue · Esc cancel",
                58,
                13,
            );
        }
        Prompt::ArchiveName {
            input, cursor, ..
        } => cursor_input_modal(
            frame,
            "Create archive",
            "Archive filename:",
            input,
            *cursor,
            "create",
        ),
        Prompt::ArchiveActions { archive, selected } => {
            let actions = ["Inspect contents", "Extract archive"];
            let body = actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    format!("{} {action}", if index == *selected { ">" } else { " " })
                })
                .collect::<Vec<_>>()
                .join("\n");
            message_modal(
                frame,
                "Archive actions",
                &format!("{}\n\n{body}", archive.display()),
                "↑/↓ choose · Enter continue · Esc cancel",
                72,
                14,
            );
        }
        Prompt::ArchiveDestination {
            input, cursor, ..
        } => cursor_input_modal(
            frame,
            "Extract archive",
            "Destination directory:",
            input,
            *cursor,
            "extract",
        ),
        Prompt::ConfirmTrash { paths } => {
            let mut body = format!("Move {} item(s) to trash?\n\n", paths.len());
            for path in paths.iter().take(6) {
                body.push_str(&format!("{}\n", path.display()));
            }
            if paths.len() > 6 {
                body.push_str(&format!("… and {} more\n", paths.len() - 6));
            }
            message_modal(
                frame,
                "Confirm move to trash",
                &body,
                &format!(
                    "{}/Enter confirm · {}/Esc cancel",
                    app.config.hotkeys.confirm_yes.display(),
                    app.config.hotkeys.confirm_no.display()
                ),
                70,
                16,
            );
        }
        Prompt::ConfirmOverwrite { sources, .. } => {
            let conflicts = sources
                .iter()
                .filter_map(|source| source.file_name())
                .map(|name| name.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n");
            message_modal(
                frame,
                "Destination already exists",
                &format!("Existing destination items will be moved to trash before replacement.\n\n{conflicts}"),
                &format!(
                    "{}/Enter overwrite · {} skip conflicts · {}/Esc abort",
                    app.config.hotkeys.overwrite.display(),
                    app.config.hotkeys.skip.display(),
                    app.config.hotkeys.abort.display()
                ),
                76,
                16,
            );
        }
        Prompt::ConfirmRestore { entries, .. } => {
            let mut body = format!("Restore {} item(s)?\n\n", entries.len());
            append_trash_names(&mut body, entries);
            message_modal(
                frame,
                "Confirm restore",
                &body,
                &format!(
                    "{}/Enter restore · Esc cancel",
                    app.config.hotkeys.restore.display()
                ),
                76,
                16,
            );
        }
        Prompt::ConfirmPermanentDelete {
            entries,
            clear_all,
            total_bytes,
            ..
        } => {
            let mut body = if *clear_all {
                format!(
                    "Permanently delete all {} trash item(s)?\n\nThis will clear the entire trash bin.\n\n",
                    entries.len()
                )
            } else {
                format!("Permanently delete {} item(s)?\n\n", entries.len())
            };
            body.push_str(&format!("Total size: {}\n\n", human_size(*total_bytes)));
            append_trash_names(&mut body, entries);
            body.push_str("\nThis cannot be undone.");
            message_modal(
                frame,
                if *clear_all {
                    "Clear entire trash bin"
                } else {
                    "Permanently delete from trash"
                },
                &body,
                &format!(
                    "{}/Enter permanently delete · Esc cancel",
                    app.config.hotkeys.permanent_delete.display()
                ),
                78,
                18,
            );
        }
        Prompt::ConfirmLuks { title, body, .. } => {
            message_modal(frame, title, body, "Enter continue · Esc cancel", 80, 17)
        }
        Prompt::LuksPassphrase {
            source,
            label,
            size,
            input,
            error,
        } => {
            let area = centered(frame.area(), 80, 16);
            draw_popup_halo(frame, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Unlock and mount LUKS volume ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(4),
                    Constraint::Length(3),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "Device: {}\nLabel: {}\nSize: {}",
                    source.display(),
                    label.as_deref().unwrap_or("—"),
                    human_size(*size),
                )),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(format!("> {}", "•".repeat(input.character_count())))
                    .block(Block::default().borders(Borders::ALL).title(" Passphrase "))
                    .style(Style::default().fg(ACCENT)),
                rows[1],
            );
            let instruction = error
                .as_deref()
                .map(|message| format!("{message}\nThe volume remains locked."))
                .unwrap_or_else(|| "Enter your passphrase to unlock this volume.".into());
            frame.render_widget(
                Paragraph::new(instruction)
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(MUTED)),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new("Enter unlock · Esc cancel")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(ACCENT)),
                rows[3],
            );
        }
        Prompt::PartitionAuthentication {
            action,
            input,
            error,
            ..
        } => {
            let area = responsive_centered(frame.area(), 72, 56, 90, 13);
            frame.render_widget(Clear, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Administrator authentication ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(4),
                    Constraint::Length(3),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "{}\n{}\n\nAdministrator password required.",
                    action.title(),
                    action.target().path.display()
                )),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(format!("> {}", "•".repeat(input.character_count())))
                    .block(Block::default().borders(Borders::ALL).title(" Password "))
                    .style(Style::default().fg(ACCENT)),
                rows[1],
            );
            frame.render_widget(
                Paragraph::new(
                    error.as_deref().unwrap_or(
                        "Sent only to sudo.",
                    ),
                )
                .style(Style::default().fg(if error.is_some() {
                    Color::Red
                } else {
                    MUTED
                })),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new("Enter authenticate · Esc cancel")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(ACCENT)),
                rows[3],
            );
        }
        Prompt::PartitionError { body, .. } => {
            partition_error_modal(frame, body);
        }
        Prompt::Mounted { path } => message_modal(
            frame,
            "Volume mounted",
            &format!("Mounted successfully:\n\n{}", path.display()),
            "[Enter] Open volume   [Esc] Stay in current directory",
            78,
            12,
        ),
        Prompt::SmbAddress {
            input,
            cursor,
            error,
        } => cursor_input_modal_with_error(
            frame,
            "Add Samba share",
            "Share address (smb://server/share):",
            input,
            *cursor,
            error.as_deref(),
            "continue",
        ),
        Prompt::SmbUsername {
            address,
            input,
            cursor,
            error,
        } => cursor_input_modal_with_error(
            frame,
            "Samba account",
            &format!(
                "{}\nUsername (leave empty for anonymous):",
                address.uri
            ),
            input,
            *cursor,
            error.as_deref(),
            "continue",
        ),
        Prompt::SmbDomain {
            address,
            input,
            cursor,
            ..
        } => cursor_input_modal_with_error(
            frame,
            "Samba domain",
            &format!("{}\nDomain or workgroup (optional):", address.uri),
            input,
            *cursor,
            None,
            "continue",
        ),
        Prompt::SmbPassword {
            address,
            username,
            domain,
            input,
            error,
        } => {
            let account = if domain.is_empty() {
                username.clone()
            } else {
                format!("{domain}\\{username}")
            };
            let body = error
                .as_deref()
                .map(|error| format!("{error}\nEnter the password again."))
                .unwrap_or_else(|| format!("Share: {}\nAccount: {account}", address.uri));
            secret_input_modal(
                frame,
                "Samba password",
                &body,
                input.character_count(),
                "Enter continue · Esc cancel",
            );
        }
        Prompt::SmbRemember { available, .. } => {
            let footer = if *available {
                format!(
                    "{} remember · {}/Enter this session only · Esc cancel",
                    app.config.hotkeys.confirm_yes.display(),
                    app.config.hotkeys.confirm_no.display()
                )
            } else {
                "Enter this session only · Esc cancel".into()
            };
            message_modal(
                frame,
                "Remember this share?",
                if *available {
                    "Save the password in your desktop's secure credential service?\n\nThe share address and account name will be saved in minfm's configuration directory."
                } else {
                    "Secure credential storage is unavailable.\n\nThe connection will only last for this login session."
                },
                &footer,
                76,
                14,
            )
        }
        Prompt::ConfirmSmbDisconnect { share } => message_modal(
            frame,
            "Disconnect network share",
            &format!("Disconnect {}?\n\nOpen files on this share may prevent disconnection.", share.address.uri),
            &format!(
                "{}/Enter disconnect · Esc cancel",
                app.config.hotkeys.network_disconnect.display()
            ),
            76,
            13,
        ),
        Prompt::ConfirmSmbForget { share } => message_modal(
            frame,
            "Forget network share",
            &format!(
                "Forget {} and remove its saved password?\n\nAn active connection will remain connected.",
                share.address.uri
            ),
            &format!(
                "{}/Enter forget · Esc cancel",
                app.config.hotkeys.network_forget.display()
            ),
            76,
            13,
        ),
        Prompt::SmbMounted { address, path } => message_modal(
            frame,
            "Network share connected",
            &format!("{}\n\n{}", address.uri, path.display()),
            "[Enter] Open share   [Esc] Stay in current directory",
            78,
            13,
        ),
        Prompt::SmbMessage { title, body, .. } => {
            message_modal(frame, title, body, "Enter/Esc close", 76, 14)
        }
        Prompt::UpdateAvailable { current, latest } => message_modal(
            frame,
            "Update available",
            &format!("Installed: {current}\nLatest:    {latest}"),
            "[Enter] Update now   [Esc] Continue",
            62,
            11,
        ),
        Prompt::Message { title, body } => {
            message_modal(frame, title, body, "Enter/Esc close", 72, 12)
        }
        Prompt::SmartReport { body, scroll, .. } => smart_report_modal(frame, body, *scroll),
        Prompt::OpenError { body, .. } => message_modal(
            frame,
            "Unable to open file",
            body,
            "Enter/Esc close",
            72,
            12,
        ),
        Prompt::Summary { summary, .. } => {
            let mut body = format!(
                "Completed: {}\nFailed: {}\nWarnings: {}\nCancelled: {}",
                summary.completed,
                summary.failed.len(),
                summary.warnings.len(),
                if summary.cancelled { "yes" } else { "no" },
            );
            for (path, error) in summary.failed.iter().take(4) {
                body.push_str(&format!("\n\n{}\n  {}", path.display(), error));
            }
            for warning in summary
                .warnings
                .iter()
                .take(4usize.saturating_sub(summary.failed.len().min(4)))
            {
                body.push_str(&format!("\n\nWarning\n  {warning}"));
            }
            message_modal(frame, "Operation summary", &body, "Enter/Esc close", 80, 20);
        }
    }
}

fn draw_progress_modal(frame: &mut Frame, app: &App) {
    let area = centered(frame.area(), 72, 13);
    frame.render_widget(Clear, area);
    let inner = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", app.progress.label))
        .inner(area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", app.progress.label)),
        area,
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(2),
            Constraint::Length(1),
        ])
        .split(inner);
    let ratio = if app.progress.total_bytes > 0 {
        app.progress.completed_bytes as f64 / app.progress.total_bytes as f64
    } else if app.progress.total_items > 0 {
        app.progress.completed_items as f64 / app.progress.total_items as f64
    } else {
        0.0
    };
    if app.progress.total_items == 0 && app.progress.total_bytes == 0 {
        let phase = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| (duration.as_millis() / 180) as usize % 4)
            .unwrap_or(0);
        let mut squares = ["□", "□", "□", "□"];
        squares[phase] = "■";
        frame.render_widget(
            Paragraph::new(format!("{}  Working…", squares.join(" ")))
                .alignment(Alignment::Center)
                .style(Style::default().fg(ACCENT)),
            rows[0],
        );
    } else {
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(ACCENT))
                .ratio(ratio.clamp(0.0, 1.0))
                .label(format!(
                    "{} / {} items",
                    app.progress.completed_items, app.progress.total_items
                )),
            rows[0],
        );
    }
    let now = std::time::Instant::now();
    if let Some(started_at) = app.progress.started_at {
        let phase_started_at = app.progress.phase_started_at.unwrap_or(started_at);
        frame.render_widget(
            Paragraph::new(
                app.progress
                    .phase
                    .as_deref()
                    .unwrap_or("Preparing device operation"),
            )
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
            rows[1],
        );
        let current = app
            .progress
            .current
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Preparing…".into());
        frame.render_widget(
            Paragraph::new(format!(
                "{current}\nElapsed: {} · phase: {}",
                format_elapsed(now.saturating_duration_since(started_at)),
                format_elapsed(now.saturating_duration_since(phase_started_at)),
            ))
            .wrap(Wrap { trim: false }),
            rows[2],
        );
    } else {
        frame.render_widget(
            Paragraph::new(format!(
                "{} / {}",
                human_size(app.progress.completed_bytes),
                human_size(app.progress.total_bytes)
            )),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(
                app.progress
                    .current
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Preparing…".into()),
            )
            .wrap(Wrap { trim: false }),
            rows[2],
        );
    }
    let device_slow = app.progress.started_at.is_some_and(|started| {
        now.saturating_duration_since(started) >= std::time::Duration::from_secs(30)
    });
    frame.render_widget(
        Paragraph::new(if app.progress.cancelling {
            "Cancellation requested…"
        } else if app.progress.cancellable {
            "Esc requests cancellation"
        } else if device_slow {
            "Still working · some devices take longer · do not disconnect"
        } else {
            "Please wait · this disk operation cannot be interrupted safely"
        })
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED)),
        rows[3],
    );
}

fn draw_search_progress(frame: &mut Frame, app: &App) {
    let area = centered(frame.area(), 76, 12);
    frame.render_widget(Clear, area);
    let scope = app.search_results.as_ref().map(|view| view.request.scope());
    let scope_label = match scope {
        Some(crate::search::SearchScope::CurrentDirectory) => "Current directory",
        Some(crate::search::SearchScope::RecursiveHere) => "Recursive here",
        Some(crate::search::SearchScope::Filesystem) => "Entire filesystem",
        None => "Search",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {scope_label} search "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let root = app
        .search_results
        .as_ref()
        .map(|view| view.request.root().display().to_string())
        .unwrap_or_else(|| app.current_dir.display().to_string());
    let message = if app.search_cancelling {
        "Cancellation requested…"
    } else {
        "Searching…"
    };
    let body = format!(
        "{}\nScope: {}\nRoot: {}\n\nMatches: {}\nSkipped: {}\n\n{}",
        message,
        scope_label,
        root,
        app.search_matches,
        app.search_skipped,
        if app.search_cancelling {
            "Waiting for worker to stop"
        } else {
            "Esc cancel"
        }
    );
    frame.render_widget(
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        inner,
    );
}

fn draw_search_form(frame: &mut Frame, app: &App, form: &SearchForm) {
    if form.advanced {
        draw_advanced_search(frame, app, form);
        return;
    }
    draw_quick_search(frame, app, form);
}

fn draw_quick_search(frame: &mut Frame, app: &App, form: &SearchForm) {
    let title = "Search";
    let area = centered(frame.area(), 72, 11);
    draw_popup_halo(frame, area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let body = match &form.error {
        Some(error) => format!(
            "{}\n\n{}\n\nEnter search · Esc cancel",
            form.draft.name, error
        ),
        None => format!(
            "{}\n\nEnter search · {} advanced · Esc cancel",
            form.draft.name,
            app.config.hotkeys.search_filesystem.display()
        ),
    };
    frame.render_widget(
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_advanced_search(frame: &mut Frame, app: &App, form: &SearchForm) {
    let screen = frame.area();
    let area = centered(
        screen,
        screen.width.saturating_sub(4).min(110),
        screen.height.saturating_sub(4).min(32),
    );
    draw_popup_halo(frame, area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Advanced search ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(if area.height < 16 { 1 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(if form.error.is_some() { 2 } else { 1 }),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "Query  {}",
            cursor_text(&form.draft.name, form.cursors.name, "<name or pattern>")
        ))
        .block(Block::default().borders(Borders::BOTTOM)),
        rows[0],
    );
    let columns = Layout::horizontal([
        Constraint::Length(if area.width < 90 { 14 } else { 20 }),
        Constraint::Min(20),
    ])
    .split(rows[1]);
    let sections = [
        (crate::app::SearchSection::Scope, "Scope"),
        (crate::app::SearchSection::Match, "Match"),
        (crate::app::SearchSection::Filters, "Filters"),
        (crate::app::SearchSection::Traversal, "Traversal"),
    ];
    let navigation = sections
        .into_iter()
        .map(|(section, label)| {
            if section == form.section {
                format!("> {label}")
            } else {
                format!("  {label}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    frame.render_widget(
        Paragraph::new(navigation).block(Block::default().borders(Borders::RIGHT)),
        columns[0],
    );
    let control_area = columns[1].inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    let (controls, active_line) = if area.height < 16 {
        (active_search_control_text(form), 0)
    } else {
        search_section_text(form)
    };
    let scroll = active_line.saturating_sub(usize::from(control_area.height).saturating_sub(1));
    frame.render_widget(
        Paragraph::new(controls)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        control_area,
    );
    if let Some(error) = &form.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(ACCENT)),
            rows[2],
        );
    }
    frame.render_widget(
        Paragraph::new(if area.width < 90 {
            format!(
                "Tab field · Enter search · Esc {}",
                match form.return_to {
                    crate::app::SearchReturn::Browser => "cancel",
                    crate::app::SearchReturn::Results => "results",
                }
            )
        } else {
            format!(
                "↑/↓ section · Tab field · ←/→ choice · Space toggle · Enter search · Esc {}",
                match form.return_to {
                    crate::app::SearchReturn::Browser => "cancel",
                    crate::app::SearchReturn::Results => "results",
                }
            )
        })
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED)),
        rows[3],
    );
    let _ = app;
}

fn active_search_control_text(form: &SearchForm) -> String {
    use crate::search::{ContentMode, NameMode, ResultLimit, SearchScope};
    match (form.section, form.field) {
        (crate::app::SearchSection::Scope, _) => format!(
            "> Scope: {}",
            match form.draft.scope {
                SearchScope::CurrentDirectory => "Current directory",
                SearchScope::RecursiveHere => "Recursive here",
                SearchScope::Filesystem => "Entire filesystem",
            }
        ),
        (crate::app::SearchSection::Match, 0) => format!(
            "> Name mode: {}",
            match form.draft.name_mode {
                NameMode::Smart => "Smart",
                NameMode::Glob => "Glob",
                NameMode::Regex => "Regex",
            }
        ),
        (crate::app::SearchSection::Match, 1) => format!(
            "> Content: {}",
            cursor_text(&form.draft.content, form.cursors.content, "<optional>")
        ),
        (crate::app::SearchSection::Match, _) => format!(
            "> Content mode: {}",
            match form.draft.content_mode {
                ContentMode::Literal => "Literal",
                ContentMode::Regex => "Regex",
            }
        ),
        (crate::app::SearchSection::Filters, 0) => format!(
            "> {} Files",
            selected(form.draft.types.contains(EntryKind::File))
        ),
        (crate::app::SearchSection::Filters, 1) => format!(
            "> {} Directories",
            selected(form.draft.types.contains(EntryKind::Directory))
        ),
        (crate::app::SearchSection::Filters, 2) => format!(
            "> {} Symlinks",
            selected(form.draft.types.contains(EntryKind::Symlink))
        ),
        (crate::app::SearchSection::Filters, 3) => format!(
            "> {} Block devices",
            selected(form.draft.types.contains(EntryKind::BlockDevice))
        ),
        (crate::app::SearchSection::Filters, 4) => format!(
            "> {} Other",
            selected(form.draft.types.contains(EntryKind::Other))
        ),
        (crate::app::SearchSection::Filters, 5) => format!(
            "> Minimum size: {}",
            cursor_text(&form.draft.minimum_size, form.cursors.minimum_size, "—")
        ),
        (crate::app::SearchSection::Filters, 6) => format!(
            "> Maximum size: {}",
            cursor_text(&form.draft.maximum_size, form.cursors.maximum_size, "—")
        ),
        (crate::app::SearchSection::Filters, 7) => format!(
            "> Modified after: {}",
            cursor_text(&form.draft.modified_after, form.cursors.modified_after, "—")
        ),
        (crate::app::SearchSection::Filters, 8) => format!(
            "> Modified before: {}",
            cursor_text(
                &form.draft.modified_before,
                form.cursors.modified_before,
                "—"
            )
        ),
        (crate::app::SearchSection::Filters, _) => format!(
            "> Include ignored/hidden: {}",
            if form.draft.include_ignored_hidden {
                "Yes"
            } else {
                "No"
            }
        ),
        (crate::app::SearchSection::Traversal, _) => format!(
            "> Result limit: {}",
            match form.draft.result_limit {
                ResultLimit::OneThousand => "1,000",
                ResultLimit::FiveThousand => "5,000",
                ResultLimit::TenThousand => "10,000",
            }
        ),
    }
}

fn search_section_text(form: &SearchForm) -> (String, usize) {
    use crate::search::{ContentMode, NameMode, ResultLimit, SearchScope};
    let mark = |field: usize| if form.field == field { ">" } else { " " };
    match form.section {
        crate::app::SearchSection::Scope => (format!(
            "Scope\n{} Scope: {} Current directory / {} Recursive here / {} Entire filesystem\nRoot: {}",
            mark(0),
            selected(form.draft.scope == SearchScope::CurrentDirectory),
            selected(form.draft.scope == SearchScope::RecursiveHere),
            selected(form.draft.scope == SearchScope::Filesystem),
            form.draft.root.display()
        ), 1),
        crate::app::SearchSection::Match => (format!(
            "Match\n{} Name mode: {}\n{} Content: {}\n{} Content mode: {}",
            mark(0),
            match form.draft.name_mode { NameMode::Smart => "Smart", NameMode::Glob => "Glob", NameMode::Regex => "Regex" },
            mark(1), cursor_text(&form.draft.content, form.cursors.content, "<optional content>"),
            mark(2),
            match form.draft.content_mode { ContentMode::Literal => "Literal", ContentMode::Regex => "Regex" },
        ), form.field + 1),
        crate::app::SearchSection::Filters => (format!(
            "Filters\n{} {} Files\n{} {} Directories\n{} {} Symlinks\n{} {} Block devices\n{} {} Other\n{} Minimum size: {}\n{} Maximum size: {}\n{} Modified after: {}\n{} Modified before: {}\n{} Include ignored/hidden: {}",
            mark(0), selected(form.draft.types.contains(EntryKind::File)), mark(1), selected(form.draft.types.contains(EntryKind::Directory)), mark(2), selected(form.draft.types.contains(EntryKind::Symlink)),
            mark(3), selected(form.draft.types.contains(EntryKind::BlockDevice)), mark(4), selected(form.draft.types.contains(EntryKind::Other)),
            mark(5), cursor_text(&form.draft.minimum_size, form.cursors.minimum_size, "—"),
            mark(6), cursor_text(&form.draft.maximum_size, form.cursors.maximum_size, "—"),
            mark(7), cursor_text(&form.draft.modified_after, form.cursors.modified_after, "—"),
            mark(8), cursor_text(&form.draft.modified_before, form.cursors.modified_before, "—"),
            mark(9),
            if form.draft.include_ignored_hidden { "Yes" } else { "No" }
        ), form.field + 1),
        crate::app::SearchSection::Traversal => (format!(
            "Traversal\n{} Result limit: {}",
            mark(0),
            match form.draft.result_limit { ResultLimit::OneThousand => "1,000", ResultLimit::FiveThousand => "5,000", ResultLimit::TenThousand => "10,000" }
        ), 1),
    }
}

fn selected(value: bool) -> &'static str {
    if value {
        "[x]"
    } else {
        "[ ]"
    }
}
fn cursor_text(value: &str, cursor: usize, hint: &str) -> String {
    if value.is_empty() {
        return format!("│{hint}");
    }
    let index = value
        .char_indices()
        .nth(cursor)
        .map_or(value.len(), |(index, _)| index);
    format!("{}│{}", &value[..index], &value[index..])
}

fn draw_update_progress(frame: &mut Frame) {
    let area = centered(frame.area(), 68, 10);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Updating minfm ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let phase = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 180) as usize % 4)
        .unwrap_or(0);
    let mut squares = ["□", "□", "□", "□"];
    squares[phase] = "■";
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n\nDownloading and verifying the update…\n\nPlease wait",
            squares.join(" ")
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT)),
        inner,
    );
}

fn draw_search_results(frame: &mut Frame, app: &App, view: &SearchView) {
    let screen = frame.area();
    let reserved = 1_u16.saturating_add(shortcut_bar_height(app, screen.width));
    let area = Rect {
        height: screen.height.saturating_sub(reserved),
        ..screen
    };
    frame.render_widget(Clear, area);
    let regions = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let table_area = regions[0];
    let footer_area = regions[1];
    let visible_count = usize::from(table_area.height.saturating_sub(3));
    let start = viewport_start(view.selected, view.results.len(), visible_count);
    let end = (start + visible_count).min(view.results.len());
    let wide = area.width >= 100;
    let rows = view
        .results
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|hit| {
            let mark = if hit.entry.selected { "*" } else { " " };
            let kind = match hit.entry.kind {
                EntryKind::Directory => "Directory",
                EntryKind::File => "File",
                EntryKind::Symlink => "Symlink",
                EntryKind::BlockDevice => "Device",
                EntryKind::Other => "Other",
            };
            if wide {
                Row::new([
                    Cell::from(mark),
                    Cell::from(hit.entry.path.display().to_string()),
                    Cell::from(kind),
                    Cell::from(hit.entry.size_text()),
                    Cell::from(hit.entry.modified_text()),
                ])
            } else {
                Row::new([
                    Cell::from(mark),
                    Cell::from(hit.entry.path.display().to_string()),
                    Cell::from(kind),
                ])
            }
        });
    let constraints = if wide {
        vec![
            Constraint::Length(2),
            Constraint::Min(24),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(19),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Min(20),
            Constraint::Length(10),
        ]
    };
    let headers = if wide {
        vec!["", "Name / path", "Type", "Size", "Modified"]
    } else {
        vec!["", "Name / path", "Type"]
    };
    let state_label = match (view.truncated, view.incomplete) {
        (true, true) => " · truncated · incomplete",
        (true, false) => " · truncated",
        (false, true) => " · incomplete",
        (false, false) => "",
    };
    let table = Table::new(rows, constraints)
        .header(Row::new(headers).style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)))
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Search results · {} match(es){} ",
            view.results.len(),
            state_label
        )));
    let selected = (!view.results.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, table_area, &mut state);
    let footer = if view.truncated {
        format!(
            "{} result limit reached · ↑/↓ {}/{} Move · Enter open · Esc return",
            format_count(view.request.result_limit().get()),
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display()
        )
    } else if view.skipped == 0 {
        format!(
            "↑/↓ {}/{} Move · Enter open · {} Search here · {} Search filesystem · Esc return",
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display(),
            app.config.hotkeys.search.display(),
            app.config.hotkeys.search_filesystem.display()
        )
    } else {
        format!(
            "↑/↓ {}/{} Move · Enter open · Esc return · {} permission error(s) skipped",
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display(),
            view.skipped
        )
    };
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(MUTED)),
        footer_area,
    );
}

fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn draw_archive(frame: &mut Frame, app: &App, view: &ArchiveView) {
    let screen = frame.area();
    let reserved = 1_u16.saturating_add(shortcut_bar_height(app, screen.width));
    let area = Rect {
        height: screen.height.saturating_sub(reserved),
        ..screen
    };
    frame.render_widget(Clear, area);
    let visible_count = usize::from(area.height.saturating_sub(3)).max(1);
    let start = viewport_start(view.selected, view.entries.len(), visible_count);
    let end = (start + visible_count).min(view.entries.len());
    let rows = view
        .entries
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            Row::new([
                Cell::from(entry.path.display().to_string()),
                Cell::from(entry.kind.label()),
                Cell::from(if entry.kind == crate::archive::ArchiveEntryKind::File {
                    human_size(entry.size)
                } else {
                    String::new()
                }),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(["Path", "Type", "Size"])
            .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
    .highlight_symbol("> ")
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Archive contents · {} · {} item(s) ",
        view.archive.display(),
        view.entries.len()
    )));
    let selected = (!view.entries.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn append_trash_names(body: &mut String, entries: &[crate::trash::TrashEntry]) {
    for entry in entries.iter().take(6) {
        body.push_str(&format!("{}\n", entry.display_name()));
    }
    if entries.len() > 6 {
        body.push_str(&format!("… and {} more\n", entries.len() - 6));
    }
}

fn draw_trash(frame: &mut Frame, app: &App, view: &TrashView) {
    let screen = frame.area();
    let area = Rect {
        x: screen.x.saturating_add(1),
        y: screen.y.saturating_add(1),
        width: screen.width.saturating_sub(2).max(1),
        height: screen.height.saturating_sub(2).max(1),
    };
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(area);
    let visible_count = usize::from(sections[0].height.saturating_sub(3)).max(1);
    let start = viewport_start(view.selected, view.entries.len(), visible_count);
    let end = (start + visible_count).min(view.entries.len());
    let rows = view.entries[start..end].iter().map(|entry| {
        Row::new(vec![
            Cell::from(if view.marked.contains(&entry.trashed_path) {
                "●"
            } else {
                " "
            }),
            Cell::from(entry.display_name()),
            Cell::from(entry.deleted_text()),
            Cell::from(entry.original_path.display().to_string()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Percentage(45),
            Constraint::Length(19),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(["", "Name", "Deleted", "Original path"]).style(Style::default().fg(MUTED)))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ")
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Trash · {} item(s) · {} ",
        view.entries.len(),
        view.manager.root().display()
    )));
    let selected = (!view.entries.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new(format!(
            "{} select │ Enter/{} restore │ {} permanent delete │ {} quick permanent delete\n{} clear trash │ {}/Esc return",
            app.config.hotkeys.select.display(), app.config.hotkeys.restore.display(),
            app.config.hotkeys.permanent_delete.display(), app.config.hotkeys.quick_permanent_delete.display(),
            app.config.hotkeys.clear_trash.display(), app.config.hotkeys.trash_bin.display()
        ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_apps(frame: &mut Frame, app: &App, view: &AppsView) {
    let area = apps_area(frame.area());
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(area);
    let rows = BuiltinApp::ALL.into_iter().map(|builtin| {
        let status = match builtin {
            BuiltinApp::DeviceManager
                if app.config.behavior.read_only || !app.device_manager_available() =>
            {
                "Unavailable"
            }
            BuiltinApp::NetworkShares if !app.network_shares_available() => "Unavailable",
            _ => "Available",
        };
        Row::new([builtin.name(), builtin.description(), status])
    });
    let table = Table::new(rows, apps_table_widths(area.width))
        .header(Row::new(["App", "Purpose", "State"]).style(Style::default().fg(MUTED)))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(" Apps "));
    let mut state = TableState::default().with_selected(Some(view.selected));
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new(format!(
            "↑/{} ↓/{} move · Enter open · {}/{}/Esc close",
            app.config.hotkeys.up.display(),
            app.config.hotkeys.down.display(),
            app.config.hotkeys.tools.display(),
            app.config.hotkeys.quit.display()
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn apps_area(area: Rect) -> Rect {
    responsive_centered(area, 88, 68, 150, 12)
}

fn draw_partitions(frame: &mut Frame, app: &App, view: &PartitionView) {
    let area = responsive_centered(frame.area(), 96, 72, 150, 92);
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(10),
            Constraint::Length(3),
        ])
        .split(area);
    let visible_count = usize::from(sections[0].height.saturating_sub(3)).max(1);
    let start = viewport_start(view.selected, view.entries.len(), visible_count);
    let end = (start + visible_count).min(view.entries.len());
    let rows = view.entries[start..end].iter().map(|entry| {
        let device = &entry.device;
        let branch = if entry.depth == 0 { "" } else { "└─ " };
        let name = format!(
            "{}{}{}",
            "  ".repeat(entry.depth.saturating_sub(1)),
            branch,
            device.name()
        );
        let filesystem = device.filesystem.as_deref().unwrap_or("—");
        let label = device
            .label
            .as_deref()
            .or(device.partition_label.as_deref())
            .unwrap_or("—");
        let mountpoints = if device.mountpoints.is_empty() {
            "—".into()
        } else {
            device
                .mountpoints
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let style = if entry.protected {
            Style::default().fg(Color::Gray)
        } else if device.is_disk() {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(name),
            Cell::from(device.kind.clone()),
            Cell::from(human_size(device.size)),
            Cell::from(filesystem.to_owned()),
            Cell::from(label.to_owned()),
            Cell::from(entry.state_text()),
            Cell::from(mountpoints),
        ])
        .style(style)
    });
    let title = if app.partition_refreshing {
        format!(
            " Device manager · refreshing · {} device(s) ",
            view.entries.len()
        )
    } else {
        format!(" Device manager · {} device(s) ", view.entries.len())
    };
    let table = Table::new(rows, partition_table_widths(area.width))
        .header(
            Row::new([
                "Device",
                "Type",
                "Size",
                "Filesystem",
                "Label",
                "State",
                "Mountpoint",
            ])
            .style(Style::default().fg(MUTED)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(title));
    let selected = (!view.entries.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, sections[0], &mut state);

    let details = view
        .entries
        .get(view.selected)
        .map(|entry| partition_details(entry, view))
        .unwrap_or_else(|| {
            if app.partition_refreshing {
                "Discovering block devices…".into()
            } else {
                "No block devices were discovered.".into()
            }
        });
    frame.render_widget(
        Paragraph::new(details).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Selected device "),
        ),
        sections[1],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "Enter/{} actions · {} refresh · ↑/{} ↓/{} move · {}/Esc apps · {} browser",
            app.config.hotkeys.partition_actions.display(),
            app.config.hotkeys.refresh.display(),
            app.config.hotkeys.up.display(),
            app.config.hotkeys.down.display(),
            app.config.hotkeys.tools.display(),
            app.config.hotkeys.quit.display()
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[2],
    );
    if let Some(overlay) = &view.overlay {
        draw_partition_overlay(frame, app, view, overlay);
    }
}

fn draw_partition_overlay(
    frame: &mut Frame,
    app: &App,
    view: &PartitionView,
    overlay: &PartitionOverlay,
) {
    match overlay {
        PartitionOverlay::Actions { selected } => {
            let tasks = app.partition_tasks_for_view(view);
            let height = (tasks.len() as u16 + 7).clamp(10, 18);
            let area = responsive_centered(frame.area(), 92, 64, 150, height);
            draw_popup_halo(frame, area);
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(3)])
                .split(area);
            let rows = tasks.into_iter().map(|task| {
                let (state, style) = match app.partition_task_unavailable(view, task) {
                    Some(reason) => (
                        format!("Blocked · {reason}"),
                        Style::default().fg(Color::Gray),
                    ),
                    None => ("Ready".into(), Style::default().fg(Color::White)),
                };
                let risk_style = match task.risk() {
                    "Erases data" => Style::default().fg(Color::Red),
                    "Changes layout" => Style::default().fg(Color::Gray),
                    _ => Style::default().fg(ACCENT),
                };
                Row::new(vec![
                    Cell::from(app.partition_task_name(view, task)),
                    Cell::from(app.partition_task_description(view, task)),
                    Cell::from(Span::styled(task.risk(), risk_style)),
                    Cell::from(state),
                ])
                .style(style)
            });
            let selected_device = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.display().to_string())
                .unwrap_or_else(|| "none".into());
            let table = Table::new(rows, partition_action_widths(area.width))
                .header(
                    Row::new(["Action", "What it does", "Risk", "Status"])
                        .style(Style::default().fg(MUTED)),
                )
                .row_highlight_style(
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" Choose what to do with {selected_device} ")),
                );
            let mut state = TableState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(table, sections[0], &mut state);
            frame.render_widget(
                Paragraph::new(format!(
                    "Enter continue · {}/Esc back",
                    app.config.hotkeys.partition_actions.display()
                ))
                .alignment(Alignment::Center)
                .style(Style::default().fg(ACCENT))
                .block(Block::default().borders(Borders::ALL)),
                sections[1],
            );
        }
        PartitionOverlay::FormatOptions {
            selected,
            encrypted,
        } => {
            let target = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.display().to_string())
                .unwrap_or_else(|| "selected device".into());
            draw_format_options(frame, &target, *selected, *encrypted);
        }
        PartitionOverlay::EncryptionFilesystem {
            selected,
            whole_disk,
        } => {
            let target = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.display().to_string())
                .unwrap_or_else(|| "selected storage".into());
            draw_encryption_filesystems(frame, &target, *selected, *whole_disk);
        }
        PartitionOverlay::EncryptionPassphrase {
            filesystem,
            passphrase,
            confirmation,
            confirming,
            error,
            ..
        } => draw_encryption_passphrase(
            frame,
            *filesystem,
            passphrase.character_count(),
            confirmation.character_count(),
            *confirming,
            error.as_deref(),
        ),
        PartitionOverlay::ChangePassphrase {
            old,
            new,
            confirmation,
            stage,
            error,
        } => draw_change_passphrase(
            frame,
            old.character_count(),
            new.character_count(),
            confirmation.character_count(),
            *stage,
            error.as_deref(),
        ),
        PartitionOverlay::DiskLayoutOptions {
            selected,
            overwrite,
        } => {
            let target = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.display().to_string())
                .unwrap_or_else(|| "selected disk".into());
            draw_disk_layout_options(frame, &target, *selected, *overwrite);
        }
        PartitionOverlay::FreeRegionOptions { selected } => {
            let (target, regions) = view
                .entries
                .get(view.selected)
                .map(|disk| {
                    (
                        disk.device.path.display().to_string(),
                        crate::partition::free_regions(disk, &view.entries),
                    )
                })
                .unwrap_or_else(|| ("selected disk".into(), Vec::new()));
            draw_free_region_options(frame, &target, &regions, *selected);
        }
        PartitionOverlay::PartitionSize {
            start_bytes,
            maximum_end,
            input,
            cursor,
            error,
        } => draw_partition_input(
            frame,
            "Partition size",
            &format!(
                "Available here: {}. Use max or enter a smaller size",
                crate::partition::size_input(maximum_end.saturating_sub(*start_bytes))
            ),
            input,
            *cursor,
            error.as_deref(),
            "Enter review · Esc free regions",
        ),
        PartitionOverlay::FormatLabel {
            filesystem,
            input,
            cursor,
            error,
            ..
        } => draw_partition_input(
            frame,
            "Optional label",
            &format!(
                "Formatting as {}. Leave blank if you do not want a label",
                filesystem.name()
            ),
            input,
            *cursor,
            error.as_deref(),
            "Enter review · Esc filesystems",
        ),
        PartitionOverlay::Input {
            task,
            input,
            cursor,
            hint,
            error,
        } => draw_partition_input(
            frame,
            app.partition_task_name(view, *task),
            hint,
            input,
            *cursor,
            error.as_deref(),
            "Enter review · Esc actions",
        ),
        PartitionOverlay::Confirm {
            action,
            yes_selected,
        } => draw_partition_confirmation(frame, action, *yes_selected),
    }
}

fn draw_format_options(frame: &mut Frame, target: &str, selected: usize, encrypted: bool) {
    let area = responsive_centered(frame.area(), 80, 60, 110, 17);
    draw_popup_halo(frame, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);
    let rows = Filesystem::ALL
        .into_iter()
        .enumerate()
        .map(|(index, filesystem)| {
            Row::new([
                if index < 3 { "Primary" } else { "Other" },
                filesystem.name(),
                filesystem.description(),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(14),
            Constraint::Min(30),
        ],
    )
    .header(Row::new(["Group", "Filesystem", "Best for"]).style(Style::default().fg(MUTED)))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ")
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Format {target} ")),
    );
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new(format!(
            "[{}] Password protection (LUKS2)",
            if encrypted { "x" } else { " " }
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(if encrypted { Color::White } else { MUTED }))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Press e to toggle "),
        ),
        sections[1],
    );
    frame.render_widget(
        Paragraph::new("Enter continue · Esc back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[2],
    );
}

fn draw_popup_halo(frame: &mut Frame, area: Rect) {
    let frame_area = frame.area();
    let left = area.x.saturating_sub(1).max(frame_area.x);
    let top = area.y.saturating_sub(1).max(frame_area.y);
    let right = area.right().saturating_add(1).min(frame_area.right());
    let bottom = area.bottom().saturating_add(1).min(frame_area.bottom());
    let halo = Rect::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    );
    frame.render_widget(Clear, halo);
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Rgb(0x00, 0x00, 0x00))),
        halo,
    );
}

fn draw_encryption_filesystems(frame: &mut Frame, target: &str, selected: usize, whole_disk: bool) {
    let area = responsive_centered(frame.area(), 84, 60, 115, 14);
    frame.render_widget(Clear, area);
    let sections = Layout::vertical([Constraint::Min(8), Constraint::Length(3)]).split(area);
    let rows = Filesystem::ALL
        .into_iter()
        .map(|filesystem| Row::new([filesystem.name(), filesystem.description()]));
    let title = if whole_disk {
        format!(" Filesystem inside encrypted GPT disk · {target} ")
    } else {
        format!(" Filesystem inside LUKS2 · {target} ")
    };
    let table = Table::new(rows, [Constraint::Length(18), Constraint::Min(30)])
        .header(Row::new(["Filesystem", "Best for"]).style(Style::default().fg(MUTED)))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(title));
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new(
            "Choose the filesystem stored inside encryption · Enter continue · Esc back",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_encryption_passphrase(
    frame: &mut Frame,
    filesystem: Filesystem,
    passphrase_length: usize,
    confirmation_length: usize,
    confirming: bool,
    error: Option<&str>,
) {
    let area = responsive_centered(frame.area(), 76, 58, 100, 15);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" LUKS2 encryption passphrase ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "The unlocked container will be formatted as {}. Store this passphrase safely; it cannot be recovered.",
            filesystem.name()
        ))
        .wrap(Wrap { trim: false }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(format!("> {}", "•".repeat(passphrase_length)))
            .block(Block::default().borders(Borders::ALL).title(" Passphrase "))
            .style(Style::default().fg(if confirming { MUTED } else { ACCENT })),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!("> {}", "•".repeat(confirmation_length)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Confirm passphrase "),
            )
            .style(Style::default().fg(if confirming { ACCENT } else { MUTED })),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(error.unwrap_or(if confirming {
            "Enter the same passphrase again."
        } else {
            "Use at least 8 characters, then press Enter."
        }))
        .style(Style::default().fg(if error.is_some() { Color::Red } else { MUTED })),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new("Enter continue · Esc back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[4],
    );
}

fn draw_change_passphrase(
    frame: &mut Frame,
    old_length: usize,
    new_length: usize,
    confirmation_length: usize,
    stage: u8,
    error: Option<&str>,
) {
    let area = responsive_centered(frame.area(), 72, 58, 100, 16);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Change LUKS passphrase ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("Enter the current key, then the new key twice."),
        rows[0],
    );
    for (index, (title, length, row)) in [
        (" Current passphrase ", old_length, rows[1]),
        (" New passphrase ", new_length, rows[2]),
        (" Confirm new passphrase ", confirmation_length, rows[3]),
    ]
    .into_iter()
    .enumerate()
    {
        frame.render_widget(
            Paragraph::new(format!("> {}", "•".repeat(length)))
                .block(Block::default().borders(Borders::ALL).title(title))
                .style(Style::default().fg(if index == stage as usize {
                    ACCENT
                } else {
                    MUTED
                })),
            row,
        );
    }
    frame.render_widget(
        Paragraph::new(error.unwrap_or("Passphrases are never shown or stored."))
            .style(Style::default().fg(if error.is_some() { Color::Red } else { MUTED })),
        rows[4],
    );
    frame.render_widget(
        Paragraph::new("Enter continue · Esc back").alignment(Alignment::Center),
        rows[5],
    );
}

fn draw_disk_layout_options(frame: &mut Frame, target: &str, selected: usize, overwrite: bool) {
    let area = responsive_centered(frame.area(), 80, 58, 110, 12);
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(3)])
        .split(area);
    let rows = [
        (
            "Empty",
            "Remove partitions and leave the disk without a table",
        ),
        ("GPT", "Modern default for most computers and large disks"),
        ("MBR", "Legacy format for compatibility with older systems"),
    ]
    .into_iter()
    .map(|(layout, description)| Row::new([layout, description]));
    let table = Table::new(rows, [Constraint::Length(14), Constraint::Min(30)])
        .header(Row::new(["Layout", "Result"]).style(Style::default().fg(MUTED)))
        .row_highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ")
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Disk layout for {target} ")),
        );
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new(format!(
            "w Full overwrite: {} · Enter review · Esc back",
            if overwrite { "on" } else { "off" }
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_free_region_options(
    frame: &mut Frame,
    target: &str,
    regions: &[(u64, u64)],
    selected: usize,
) {
    let height = (regions.len() as u16 + 7).clamp(10, 16);
    let area = responsive_centered(frame.area(), 82, 60, 115, height);
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(area);
    let rows = regions.iter().enumerate().map(|(index, (start, end))| {
        Row::new([
            format!("Region {}", index + 1),
            crate::partition::size_input(end.saturating_sub(*start)),
            format!(
                "{} – {}",
                crate::partition::size_input(*start),
                crate::partition::size_input(*end)
            ),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(16),
            Constraint::Min(28),
        ],
    )
    .header(Row::new(["Free space", "Size", "Position"]).style(Style::default().fg(MUTED)))
    .row_highlight_style(
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("> ")
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Create partition on {target} ")),
    );
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, sections[0], &mut state);
    frame.render_widget(
        Paragraph::new("Choose free space · Enter continue · Esc back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_partition_input(
    frame: &mut Frame,
    title: &str,
    hint: &str,
    input: &str,
    cursor: usize,
    error: Option<&str>,
    footer: &str,
) {
    let area = responsive_centered(frame.area(), 80, 56, 100, 12);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(hint).wrap(Wrap { trim: false }), rows[0]);
    draw_partition_edit_field(frame, rows[1], input, cursor);
    frame.render_widget(
        Paragraph::new(error.unwrap_or("Review the exact values before continuing."))
            .style(Style::default().fg(if error.is_some() { Color::Red } else { MUTED })),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[3],
    );
}

fn draw_partition_confirmation(
    frame: &mut Frame,
    action: &crate::partition::PartitionAction,
    yes_selected: bool,
) {
    let destructive = action.is_destructive();
    let erases_data = action.erases_data();
    let area = responsive_centered(frame.area(), 76, 52, 110, 12);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if erases_data {
            Color::Red
        } else if destructive {
            Color::Gray
        } else {
            ACCENT
        }))
        .title(format!(" Review · {} ", action.title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(action.confirmation_text()).wrap(Wrap { trim: false }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(action.warning_text())
            .alignment(Alignment::Center)
            .style(Style::default().fg(if erases_data {
                Color::Red
            } else if destructive {
                Color::Gray
            } else {
                MUTED
            })),
        rows[1],
    );
    // Keep a little breathing room between the warning and the decisions.
    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(partition_button_width(rows[3].width)),
            Constraint::Length(2),
            Constraint::Length(partition_button_width(rows[3].width)),
            Constraint::Min(0),
        ])
        .split(rows[3]);
    let button = |label, selected| {
        let (text, style) = if selected {
            (
                format!("● {label}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (format!("  {label}"), Style::default().fg(Color::DarkGray))
        };
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(style)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(if selected {
                        BorderType::Double
                    } else {
                        BorderType::Plain
                    })
                    .border_style(Style::default().fg(if selected {
                        Color::White
                    } else {
                        Color::DarkGray
                    })),
            )
    };
    frame.render_widget(button("No", !yes_selected), buttons[1]);
    frame.render_widget(button("Yes", yes_selected), buttons[3]);
    frame.render_widget(
        Paragraph::new("←/→ choose · Enter apply · Esc cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[4],
    );
}

fn partition_button_width(area_width: u16) -> u16 {
    area_width.saturating_sub(6).saturating_div(2).clamp(8, 16)
}

fn apps_table_widths(width: u16) -> Vec<Constraint> {
    if width < 90 {
        vec![
            Constraint::Length(20),
            Constraint::Min(28),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Percentage(22),
            Constraint::Min(36),
            Constraint::Percentage(26),
        ]
    }
}

fn partition_table_widths(width: u16) -> Vec<Constraint> {
    if width >= 120 {
        vec![
            Constraint::Percentage(20),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Percentage(14),
            Constraint::Length(11),
            Constraint::Min(15),
        ]
    } else if width >= 90 {
        vec![
            Constraint::Percentage(24),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Length(0),
            Constraint::Length(11),
            Constraint::Min(15),
        ]
    } else {
        vec![
            Constraint::Percentage(45),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Length(0),
            Constraint::Length(10),
            Constraint::Length(0),
        ]
    }
}

fn partition_action_widths(width: u16) -> Vec<Constraint> {
    if width < 90 {
        vec![
            Constraint::Length(20),
            Constraint::Min(28),
            Constraint::Length(0),
            Constraint::Length(0),
        ]
    } else {
        vec![
            Constraint::Percentage(22),
            Constraint::Percentage(38),
            Constraint::Length(14),
            Constraint::Min(18),
        ]
    }
}

fn draw_partition_edit_field(frame: &mut Frame, area: Rect, input: &str, cursor: usize) {
    let characters = input.chars().collect::<Vec<_>>();
    let cursor = cursor.min(characters.len());
    let visible_width = area.width.saturating_sub(6) as usize;
    let start = cursor.saturating_sub(visible_width);
    let end = (start + visible_width).min(characters.len());
    let visible = characters[start..end].iter().collect::<String>();
    frame.render_widget(
        Paragraph::new(format!("> {visible}"))
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(ACCENT)),
        area,
    );
    let cursor_x = area.x + 3 + cursor.saturating_sub(start).min(visible_width) as u16;
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn partition_details(entry: &crate::partition::PartitionEntry, view: &PartitionView) -> String {
    let device = &entry.device;
    let kind = if device.is_disk() {
        "Disk"
    } else if device.kind == "part" {
        "Partition"
    } else {
        "Device"
    };
    let mut lines = vec![format!(
        "{} · {} · {}",
        device.path.display(),
        kind,
        human_size(device.size)
    )];

    if device.is_disk() {
        let partition_count = view
            .entries
            .iter()
            .filter(|candidate| {
                candidate.device.kind == "part"
                    && candidate.device.parent.as_ref() == Some(&device.path)
            })
            .count();
        let table = device.table_type.as_ref().map_or_else(
            || "No partition table".to_string(),
            |table| table.to_ascii_uppercase(),
        );
        let free = crate::partition::largest_free_region(entry, &view.entries)
            .map(|(start, end)| format!("{} free", human_size(end.saturating_sub(start))))
            .unwrap_or_else(|| "No usable free space".into());
        lines.push(format!("{table} · {partition_count} partition(s) · {free}"));
        if let Some(model) = device.model.as_deref().filter(|model| !model.is_empty()) {
            lines.push(format!(
                "Model: {model} · Removable: {}",
                if device.removable { "yes" } else { "no" }
            ));
        }
    } else {
        let filesystem = display_or_dash(device.filesystem.as_deref());
        let label = device
            .label
            .as_deref()
            .filter(|label| !label.is_empty())
            .map(|label| format!(" · {label}"))
            .unwrap_or_default();
        lines.push(format!("{filesystem}{label}"));
        let mountpoint = device
            .mountpoints
            .first()
            .map(|path| format!("Mounted at {}", path.display()))
            .unwrap_or_else(|| "Not mounted".into());
        lines.push(mountpoint);
        if let Some(parent) = device.parent.as_ref() {
            lines.push(format!("Parent disk: {}", parent.display()));
        }
    }

    lines.push(format!("UUID: {}", display_or_dash(device.uuid.as_deref())));
    lines.push(format!("Status: {}", partition_status(entry)));
    lines.join("\n")
}

fn partition_status(entry: &crate::partition::PartitionEntry) -> &'static str {
    if entry.protected {
        "Protected system storage · changes disabled"
    } else if entry.device.read_only {
        "Read-only device · changes disabled"
    } else if entry.device.is_mounted() {
        "Mounted · unmount before changing"
    } else if entry.mounted_descendants {
        "Contains mounted storage · changes disabled"
    } else {
        "Ready"
    }
}

fn display_or_dash(value: Option<&str>) -> &str {
    value.filter(|value| !value.is_empty()).unwrap_or("—")
}

fn draw_devices(frame: &mut Frame, app: &App, view: &DeviceView) {
    let area = centered(frame.area(), 92, 80);
    frame.render_widget(Clear, area);
    let rows = view.devices.iter().map(|device| {
        Row::new(vec![
            Cell::from(format!(
                "{}{}",
                "  ".repeat((device.kind != "disk") as usize),
                device.source.display()
            )),
            Cell::from(device.label.clone().unwrap_or_else(|| "—".into())),
            Cell::from(human_size(device.size)),
            Cell::from(if device.encrypted {
                format!("LUKS · {}", device.state_text())
            } else {
                device.state_text().into()
            }),
            Cell::from(
                device
                    .mountpoints
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(20),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(["Device", "Label", "Size", "State", "Mountpoint"])
            .style(Style::default().fg(MUTED)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ")
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Storage devices · {} found ", view.devices.len())),
    );
    let mut state = TableState::default().with_selected(if view.devices.is_empty() {
        None
    } else {
        Some(view.selected)
    });
    frame.render_stateful_widget(table, area, &mut state);
    let footer = Rect {
        x: area.x + 2,
        y: area.y + area.height.saturating_sub(2),
        width: area.width.saturating_sub(4),
        height: 1,
    };
    let action = view
        .devices
        .get(view.selected)
        .map(|device| {
            if device.system_protected {
                return "Protected system volume · disk actions unavailable".to_string();
            }
            let mut action = if !device.encrypted && device.filesystem.is_none() {
                "No directly mountable filesystem".to_string()
            } else if device.encrypted && device.is_locked() {
                format!(
                    "Enter/{} unlock and mount",
                    app.config.hotkeys.device_unmount.display()
                )
            } else if device.encrypted && device.is_mounted() {
                format!(
                    "Enter/{} unmount and lock",
                    app.config.hotkeys.device_unmount.display()
                )
            } else if device.is_mounted() {
                format!(
                    "Enter/{} unmount",
                    app.config.hotkeys.device_unmount.display()
                )
            } else {
                format!("Enter/{} mount", app.config.hotkeys.device_action.display())
            };
            if device.ejectable && !device.eject_blocked {
                action.push_str(&format!(
                    " · {} eject",
                    app.config.hotkeys.device_eject.display()
                ));
            } else if device.ejectable && device.eject_blocked {
                action.push_str(" · eject unavailable: drive in use");
            }
            action
        })
        .unwrap_or_else(|| {
            if app.device_refreshing {
                "Refreshing devices…".into()
            } else {
                "No storage devices found".into()
            }
        });
    frame.render_widget(
        Paragraph::new(format!(
            "{action} · {} refresh · Esc return · {} browser",
            app.config.hotkeys.refresh.display(),
            app.config.hotkeys.quit.display()
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT)),
        footer,
    );
}

fn draw_network(frame: &mut Frame, app: &App, view: &NetworkView) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(area);
    let visible_count = usize::from(sections[0].height.saturating_sub(3)).max(1);
    let start = viewport_start(view.selected, view.shares.len(), visible_count);
    let end = (start + visible_count).min(view.shares.len());
    let rows = view.shares[start..end].iter().map(|share| {
        Row::new(vec![
            Cell::from(share.address.share.clone()),
            Cell::from(share.address.server.clone()),
            Cell::from(share.state()),
            Cell::from(share.account()),
            Cell::from(
                share
                    .mount_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(22),
            Constraint::Percentage(20),
            Constraint::Length(12),
            Constraint::Percentage(20),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(["Share", "Server", "State", "Account", "Local path"])
            .style(Style::default().fg(MUTED)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ")
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Network shares · {} found{} ",
        view.shares.len(),
        if app.network_refreshing {
            " · refreshing…"
        } else {
            ""
        }
    )));
    let selected = (!view.shares.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, sections[0], &mut state);
    let selected_share = view.shares.get(view.selected);
    let contextual = selected_share
        .map(|share| {
            let mut actions = if share.mount_path.is_some() {
                format!(
                    "Enter open · {} disconnect",
                    app.config.hotkeys.network_disconnect.display()
                )
            } else {
                "Enter connect".to_string()
            };
            if share.saved {
                actions.push_str(&format!(
                    " · {} forget",
                    app.config.hotkeys.network_forget.display()
                ));
            }
            actions
        })
        .unwrap_or_else(|| {
            format!(
                "No shares found · use {} to add one",
                app.config.hotkeys.network_add.display()
            )
        });
    frame.render_widget(
        Paragraph::new(format!(
            "{contextual}\n{} add share · {} refresh · {}/Esc return · {} browser",
            app.config.hotkeys.network_add.display(),
            app.config.hotkeys.refresh.display(),
            app.config.hotkeys.network_shares.display(),
            app.config.hotkeys.quit.display()
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_network_progress(frame: &mut Frame) {
    let area = centered(frame.area(), 70, 10);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Network share ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let phase = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 180) as usize % 4)
        .unwrap_or(0);
    let mut squares = ["□", "□", "□", "□"];
    squares[phase] = "■";
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n\nWorking with the network share…\n\nPlease wait",
            squares.join(" ")
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT)),
        inner,
    );
}

fn draw_help(frame: &mut Frame, app: &App) {
    let h = &app.config.hotkeys;
    let body = format!(
        "Navigation\n  ↑/{} ↓/{}       move\n  Enter          toggle directory or open file\n  →/{}            expand/child in tree; open in table\n  ←/{}            collapse/parent in tree; parent in table\n  {}              toggle tree/table view\n  {}              go to path\n  {}              search current directory\n  {}              search entire filesystem\n\nClipboard\n  {}              cut\n  {}              copy\n  {}              paste\n\nArchives\n  {}              create, inspect, or extract archive\n\nFiles\n  {}          select\n  {}              edit selected text file\n  {}              rename file or directory\n  {} / {}          trash with prompt / quick trash\n  {}              trash bin\n\nTrash bin\n  {}          select\n  Enter / {}      restore\n  {} / {}          permanent delete / quick permanent delete\n  {}              clear trash with confirmation\n\nCreate\n  {}              create file\n  {}              create directory\n\nTools and devices\n  {}              built-in tools launcher\n  {}              device manager\n  {}              safely eject selected removable device\n\nNetwork shares\n  {}              network shares\n  {}              add share\n  Enter          open or connect\n  {}              disconnect\n  {}              forget saved share\n  {}              refresh\n\nView\n  {}              hidden files\n  {} / {}          sort mode / reverse\n  {}              information\n  Esc            close this view",
        h.up.display(), h.down.display(), h.expand.display(), h.collapse.display(),
        h.toggle_view.display(), h.go_to.display(), h.search.display(), h.search_filesystem.display(),
        h.cut.display(), h.copy.display(), h.paste.display(), h.archive.display(), h.select.display(), h.edit.display(),
        h.rename.display(), h.trash.display(), h.quick_trash.display(), h.trash_bin.display(),
        h.select.display(), h.restore.display(), h.permanent_delete.display(),
        h.quick_permanent_delete.display(), h.clear_trash.display(), h.create_file.display(),
        h.create_directory.display(), h.tools.display(), h.devices.display(),
        h.device_eject.display(), h.network_shares.display(), h.network_add.display(),
        h.network_disconnect.display(), h.network_forget.display(), h.refresh.display(),
        h.hidden.display(), h.sort.display(), h.reverse_sort.display(), h.info.display(),
    );
    let title = format!("Help · minfm {}", env!("CARGO_PKG_VERSION"));
    message_modal(frame, &title, &body, "Esc/Enter close", 70, 94);
}

fn draw_info(frame: &mut Frame, app: &App) {
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let body = format!(
        "minfm {}\n\nBinary:\n{}\n\nConfig:\n{}\n\nCurrent directory:\n{}\n\nMode: {}\nView: {}\nSort: {} {}\n\nSystem tools:\n  lsblk: {}\n  udisksctl: {}\n  cryptsetup: {}\n  smartctl: {}\n  hdparm: {}\n  gio: {}\n  secret-tool: {}\n  parted: {}\n  wipefs: {}\n  sfdisk: {}\n  sudo: {}",
        env!("CARGO_PKG_VERSION"),
        binary,
        app.config_path.display(),
        app.current_dir.display(),
        if app.config.behavior.read_only { "read only" } else { "normal" },
        app.browser_view_label(),
        app.sort_label(),
        if app.config.ui.reverse_sort { "descending" } else { "ascending" },
        availability("lsblk"),
        availability("udisksctl"),
        availability("cryptsetup"),
        availability("smartctl"),
        availability("hdparm"),
        availability("gio"),
        availability("secret-tool"),
        availability("parted"),
        availability("wipefs"),
        availability("sfdisk"),
        availability("sudo"),
    );
    message_modal(
        frame,
        "Application information",
        &body,
        "Esc/Enter close",
        78,
        32,
    );
}

fn availability(command: &str) -> &'static str {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
        })
        .filter(|available| *available)
        .map(|_| "available")
        .unwrap_or("missing")
}

fn draw_config_error(frame: &mut Frame, app: &App, path: &std::path::Path, error: &str) {
    let body = format!(
        "minfm cannot use its configuration.\n\nFile:\n{}\n\n{}\n\nAll file interaction is disabled until the configuration is corrected and reloaded.",
        path.display(), error
    );
    message_modal(
        frame,
        "Configuration error",
        &body,
        &format!(
            "{} edit config · {} reload · {} quit",
            app.config.hotkeys.config_edit.display(),
            app.config.hotkeys.config_reload.display(),
            app.config.hotkeys.quit.display()
        ),
        84,
        24,
    );
}

fn input_modal(frame: &mut Frame, title: &str, input: &str, footer: &str) {
    let area = centered(frame.area(), 72, 9);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new("Enter a value:"), rows[0]);
    frame.render_widget(
        Paragraph::new(format!("> {input}▌"))
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(MUTED)),
        rows[2],
    );
}

fn cursor_input_modal(
    frame: &mut Frame,
    title: &str,
    prompt: &str,
    input: &str,
    cursor: usize,
    action: &str,
) {
    let area = centered(frame.area(), 72, 9);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(prompt), rows[0]);

    let characters = input.chars().collect::<Vec<_>>();
    let cursor = cursor.min(characters.len());
    let visible_width = rows[1].width.saturating_sub(6) as usize;
    let start = cursor.saturating_sub(visible_width);
    let end = (start + visible_width).min(characters.len());
    let visible = characters[start..end].iter().collect::<String>();
    frame.render_widget(
        Paragraph::new(format!("> {visible}"))
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    let cursor_x = rows[1].x + 3 + cursor.saturating_sub(start).min(visible_width) as u16;
    frame.set_cursor_position((cursor_x, rows[1].y + 1));
    frame.render_widget(
        Paragraph::new(format!(
            "←/→ move · Home/End jump · Enter {action} · Esc cancel"
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED)),
        rows[2],
    );
}

fn cursor_input_modal_with_error(
    frame: &mut Frame,
    title: &str,
    prompt: &str,
    input: &str,
    cursor: usize,
    error: Option<&str>,
    action: &str,
) {
    let area = centered(frame.area(), 78, 12);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(prompt).wrap(Wrap { trim: false }), rows[0]);

    let characters = input.chars().collect::<Vec<_>>();
    let cursor = cursor.min(characters.len());
    let visible_width = rows[1].width.saturating_sub(6) as usize;
    let start = cursor.saturating_sub(visible_width);
    let end = (start + visible_width).min(characters.len());
    let visible = characters[start..end].iter().collect::<String>();
    frame.render_widget(
        Paragraph::new(format!("> {visible}"))
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    let cursor_x = rows[1].x + 3 + cursor.saturating_sub(start).min(visible_width) as u16;
    frame.set_cursor_position((cursor_x, rows[1].y + 1));
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Red)),
            rows[2],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "←/→ move · Home/End jump · Enter {action} · Esc cancel"
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED)),
        rows[3],
    );
}

fn secret_input_modal(
    frame: &mut Frame,
    title: &str,
    body: &str,
    character_count: usize,
    footer: &str,
) {
    let area = centered(frame.area(), 78, 14);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), rows[0]);
    frame.render_widget(
        Paragraph::new(format!("> {}", "•".repeat(character_count)))
            .block(Block::default().borders(Borders::ALL).title(" Password "))
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(
            "The password is only used for this connection unless you choose to remember it.",
        )
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(MUTED)),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[3],
    );
}

fn message_modal(
    frame: &mut Frame,
    title: &str,
    body: &str,
    footer: &str,
    width: u16,
    height: u16,
) {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), rows[0]);
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
}

fn smart_report_modal(frame: &mut Frame, body: &str, scroll: u16) {
    let content_lines = body.lines().count().max(1) as u16;
    let desired_height = content_lines.saturating_add(3).clamp(6, 20);
    let area = responsive_centered(frame.area(), 84, 70, 96, desired_height);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SMART report ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let maximum_scroll = content_lines.saturating_sub(rows[0].height);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(maximum_scroll), 0)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new("↑/↓ scroll · PageUp/PageDown jump · Enter/Esc close")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
}

fn partition_error_modal(frame: &mut Frame, body: &str) {
    let area = responsive_centered(frame.area(), 76, 52, 96, 14);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Red))
        .title(
            Line::from(" Partition operation failed ")
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::White)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new("Enter/Esc return")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = if height > 50 {
        (area.height * height / 100).max(6)
    } else {
        height
    };
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn responsive_centered(
    area: Rect,
    width_percent: u16,
    minimum_width: u16,
    maximum_width: u16,
    height: u16,
) -> Rect {
    let responsive_width = area
        .width
        .saturating_mul(width_percent)
        .saturating_div(100)
        .max(minimum_width)
        .min(maximum_width);
    centered(area, responsive_width, height)
}

fn viewport_start(selected: usize, item_count: usize, visible_count: usize) -> usize {
    if item_count <= visible_count || selected < visible_count {
        0
    } else {
        (selected + 1 - visible_count).min(item_count - visible_count)
    }
}

#[cfg(test)]
mod performance_tests {
    use super::*;
    use crate::{
        app::{ArchiveView, PartitionView},
        archive::{ArchiveEntry, ArchiveEntryKind},
        config::{Config, ConfigLoad},
        network::{NetworkSecret, ShareAddress},
        partition,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::{path::PathBuf, time::Instant};

    #[test]
    fn viewport_tracks_selection_without_exceeding_bounds() {
        assert_eq!(viewport_start(0, 100, 10), 0);
        assert_eq!(viewport_start(9, 100, 10), 0);
        assert_eq!(viewport_start(10, 100, 10), 1);
        assert_eq!(viewport_start(99, 100, 10), 90);
        assert_eq!(viewport_start(0, 5, 10), 0);
    }

    #[test]
    fn footer_wraps_only_between_complete_shortcuts() {
        let lines = shortcut_lines_owned(&[("k".into(), "Move"), ("Ctrl+x".into(), "Cut")], 14, 2);
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text, [" k Move", " Ctrl+x Cut"]);
    }

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn entry_icons_are_high_contrast_without_a_background_fill() {
        let icon = icon_span("󰉋  ".into());
        assert_eq!(icon.style.fg, Some(Color::Gray));
        assert_eq!(icon.style.bg, None);
        assert!(icon.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn search_form_renders_validation_error() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        app.mode = AppMode::SearchForm(SearchForm {
            draft: crate::search::SearchDraft::quick(temp.path().to_path_buf()),
            advanced: false,
            section: crate::app::SearchSection::Match,
            field: 0,
            cursors: crate::app::SearchCursors::default(),
            error: Some("enter a search or choose a filter".into()),
            return_to: crate::app::SearchReturn::Browser,
        });
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        assert!(rendered_text(&terminal).contains("enter a search or choose a filter"));
    }

    #[test]
    fn search_advanced_wide_renders_persistent_query_and_all_sections() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        app.mode = AppMode::SearchForm(SearchForm::advanced(
            temp.path().to_path_buf(),
            crate::search::SearchScope::CurrentDirectory,
            crate::app::SearchReturn::Browser,
        ));
        let backend = TestBackend::new(140, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        for label in ["Query", "Scope", "Match", "Filters", "Traversal"] {
            assert!(text.contains(label), "missing {label}");
        }
        assert!(text.contains("Enter search"));
        assert!(text.contains("Esc cancel"));
    }

    #[test]
    fn search_advanced_narrow_keeps_active_section_error_and_footer() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        let mut form = SearchForm::advanced(
            temp.path().to_path_buf(),
            crate::search::SearchScope::CurrentDirectory,
            crate::app::SearchReturn::Browser,
        );
        form.section = crate::app::SearchSection::Filters;
        form.field = 5;
        form.error = Some("invalid minimum size: nope".into());
        app.mode = AppMode::SearchForm(form);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        for label in [
            "Filters",
            "Minimum size",
            "invalid minimum size",
            "Enter search",
            "Esc cancel",
        ] {
            assert!(text.contains(label), "missing {label}");
        }
    }

    #[test]
    fn search_advanced_last_field_is_marked_and_scrolled_at_all_supported_sizes() {
        for (width, height) in [(80, 24), (100, 30), (140, 40), (40, 12)] {
            let temp = tempfile::tempdir().unwrap();
            let mut app = App::new(
                temp.path().to_path_buf(),
                ConfigLoad::Valid {
                    config: Config::default(),
                    path: temp.path().join("config.toml"),
                },
                false,
            );
            let mut form = SearchForm::advanced(
                temp.path().to_path_buf(),
                crate::search::SearchScope::CurrentDirectory,
                crate::app::SearchReturn::Browser,
            );
            form.section = crate::app::SearchSection::Filters;
            form.field = 9;
            app.mode = AppMode::SearchForm(form);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let text = rendered_text(&terminal);
            assert!(text.contains("> Include"), "{width}x{height}: {text}");
            assert!(text.contains("ignored/hidden"), "{width}x{height}: {text}");
        }
    }

    #[test]
    fn search_progress_labels_each_scope_and_root_truthfully() {
        for (scope, label) in [
            (
                crate::search::SearchScope::CurrentDirectory,
                "Current directory",
            ),
            (crate::search::SearchScope::RecursiveHere, "Recursive here"),
            (crate::search::SearchScope::Filesystem, "Entire filesystem"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut app = App::new(
                temp.path().to_path_buf(),
                ConfigLoad::Valid {
                    config: Config::default(),
                    path: temp.path().join("config.toml"),
                },
                false,
            );
            let mut draft = crate::search::SearchDraft::advanced(temp.path().to_path_buf(), scope);
            draft.name = "needle".into();
            app.search_results = Some(crate::app::SearchView {
                request: draft.compile(true).unwrap(),
                results: Vec::new(),
                selected: 0,
                selected_path: None,
                skipped: 0,
                truncated: false,
                incomplete: false,
            });
            app.mode = AppMode::SearchProgress;
            let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let text = rendered_text(&terminal);
            assert!(text.contains(label));
            assert!(text.contains(&temp.path().display().to_string()));
        }
    }

    #[test]
    fn search_results_footer_uses_exact_truncated_limit_preset() {
        for (limit, label) in [
            (
                crate::search::ResultLimit::OneThousand,
                "1,000 result limit reached",
            ),
            (
                crate::search::ResultLimit::FiveThousand,
                "5,000 result limit reached",
            ),
            (
                crate::search::ResultLimit::TenThousand,
                "10,000 result limit reached",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut app = App::new(
                temp.path().to_path_buf(),
                ConfigLoad::Valid {
                    config: Config::default(),
                    path: temp.path().join("config.toml"),
                },
                false,
            );
            let mut draft = crate::search::SearchDraft::quick(temp.path().to_path_buf());
            draft.name = "needle".into();
            draft.result_limit = limit;
            app.search_results = Some(crate::app::SearchView {
                request: draft.compile(true).unwrap(),
                results: Vec::new(),
                selected: 0,
                selected_path: None,
                skipped: 0,
                truncated: true,
                incomplete: false,
            });
            app.mode = AppMode::SearchResults;
            let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            assert!(rendered_text(&terminal).contains(label));
        }
    }

    #[test]
    fn search_results_reserve_footer_below_last_visible_row() {
        for (width, height) in [(80, 24), (40, 12)] {
            let temp = tempfile::tempdir().unwrap();
            let mut app = App::new(
                temp.path().to_path_buf(),
                ConfigLoad::Valid {
                    config: Config::default(),
                    path: temp.path().join("config.toml"),
                },
                false,
            );
            let mut draft = crate::search::SearchDraft::quick(temp.path().to_path_buf());
            draft.name = "row".into();
            let results = (0..30)
                .map(|index| {
                    let path = temp.path().join(format!("row-{index:02}-unique"));
                    std::fs::write(&path, []).unwrap();
                    crate::search::hit_for_test(path, "row")
                })
                .collect::<Vec<_>>();
            let selected = results.len() - 1;
            app.search_results = Some(crate::app::SearchView {
                request: draft.compile(true).unwrap(),
                results,
                selected,
                selected_path: None,
                skipped: 0,
                truncated: true,
                incomplete: false,
            });
            app.mode = AppMode::SearchResults;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let rows = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let result_row = rows
                .iter()
                .position(|row| row.contains("│>"))
                .unwrap_or_else(|| {
                    panic!(
                        "last selected result visible: {}x{} {:?}",
                        width, height, rows
                    )
                });
            let footer_row = rows
                .iter()
                .position(|row| row.contains("result limit reached"))
                .expect("footer visible");
            assert_ne!(result_row, footer_row);
            assert!(result_row < footer_row);
        }
    }

    #[test]
    fn search_views_do_not_panic_on_tiny_terminals() {
        for (width, height) in [(1, 1), (10, 3), (20, 6)] {
            let temp = tempfile::tempdir().unwrap();
            let mut app = App::new(
                temp.path().to_path_buf(),
                ConfigLoad::Valid {
                    config: Config::default(),
                    path: temp.path().join("config.toml"),
                },
                false,
            );
            app.mode = AppMode::SearchForm(SearchForm::advanced(
                temp.path().to_path_buf(),
                crate::search::SearchScope::CurrentDirectory,
                crate::app::SearchReturn::Browser,
            ));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
        }
    }

    #[test]
    fn expanded_tree_renders_continuing_and_last_branch_lines() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("alpha/child")).unwrap();
        std::fs::create_dir(temp.path().join("beta")).unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);

        assert!(text.contains("├──"), "the first root entry needs a branch");
        assert!(
            text.contains("│   └──"),
            "an expanded child needs its ancestor continuation"
        );
        assert!(
            text.contains("└──"),
            "the final sibling needs an end branch"
        );
    }

    #[test]
    fn footer_and_help_expose_the_network_share_hotkey() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        let backend = TestBackend::new(150, 60);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(rendered_text(&terminal).contains("N Shares"));

        app.mode = AppMode::Help;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(rendered_text(&terminal).contains("N              network shares"));
    }

    #[test]
    fn footer_and_help_render_configured_hotkeys() {
        let temp = tempfile::tempdir().unwrap();
        let config = toml::from_str(
            "[hotkeys]\ntools = 'F2'\nnetwork_shares = 'F3'\ndevices = 'F4'\narchive = 'F5'\n",
        )
        .unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config,
                path: temp.path().join("config.toml"),
            },
            false,
        );
        let backend = TestBackend::new(150, 60);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        assert!(text.contains("F2 Tools"));
        assert!(text.contains("F3 Shares"));
        assert!(text.contains("F4"));
        assert!(text.contains("F5 Archive"));
        assert!(!text.contains("M Tools"));

        app.mode = AppMode::Help;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        assert!(text.contains("F2              built-in tools launcher"));
        assert!(text.contains("F3              network shares"));
        assert!(text.contains("F4              device manager"));
        assert!(text.contains("F5              create, inspect, or extract archive"));
    }

    #[test]
    fn archive_workflow_renders_at_narrow_and_wide_sizes() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        app.mode = AppMode::Archive(ArchiveView {
            archive: PathBuf::from("example.tar.gz"),
            entries: vec![
                ArchiveEntry {
                    path: PathBuf::from("folder"),
                    kind: ArchiveEntryKind::Directory,
                    size: 0,
                },
                ArchiveEntry {
                    path: PathBuf::from("folder/document.txt"),
                    kind: ArchiveEntryKind::File,
                    size: 4096,
                },
            ],
            selected: 1,
        });
        for width in [60, 100, 160] {
            let backend = TestBackend::new(width, 25);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let text = rendered_text(&terminal);
            assert!(text.contains("Archive contents"));
            assert!(text.contains("document.txt"));
            assert!(text.contains("4.0 KiB"));
            assert!(text.contains("2 archive items"));
            assert!(!text.contains("selected:"));
        }

        app.mode = AppMode::Prompt(Prompt::ArchiveFormat {
            sources: vec![PathBuf::from("document.txt")],
            selected: 1,
        });
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        assert!(text.contains("Create archive"));
        assert!(text.contains("> ZIP"));
    }

    #[test]
    fn header_actions_render_at_narrow_medium_and_wide_widths() {
        let temp = tempfile::tempdir().unwrap();
        let app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        for width in [50, 100, 150] {
            let backend = TestBackend::new(width, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let text = rendered_text(&terminal);
            assert!(!text.contains('󰩹'), "old trash icon at {width}");
            assert!(!text.contains('󰋼'), "old info icon at {width}");
            assert!(!text.contains('󰍹'), "old device icon at {width}");
            assert!(text.contains("Sort"), "missing sort status at {width}");
        }
    }

    #[test]
    fn removed_header_icon_overrides_do_not_change_the_text_bar() {
        let temp = tempfile::tempdir().unwrap();
        let config =
            toml::from_str("[icons.overrides]\ntrash = 'X'\ninfo = 'Y'\ndevices = 'Z'\n").unwrap();
        let app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config,
                path: temp.path().join("config.toml"),
            },
            false,
        );
        let backend = TestBackend::new(150, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        assert!(text.contains("Trash"));
        assert!(text.contains("Info"));
        assert!(text.contains("Devices"));
        assert!(!text.contains('󰋼'));
    }

    #[test]
    fn samba_password_modal_never_renders_the_password() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            false,
        );
        let mut secret = NetworkSecret::default();
        for character in "never-render-this".chars() {
            secret.push(character);
        }
        app.mode = AppMode::Prompt(Prompt::SmbPassword {
            address: ShareAddress::parse("smb://nas/private").unwrap(),
            username: "alice".into(),
            domain: String::new(),
            input: secret,
            error: None,
        });
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = rendered_text(&terminal);
        assert!(!text.contains("never-render-this"));
        assert!(text.contains("•••••••••••••••••"));
    }

    #[test]
    fn partition_modals_expand_with_the_terminal_up_to_a_readable_limit() {
        let small = responsive_centered(Rect::new(0, 0, 80, 30), 92, 64, 150, 12);
        let medium = responsive_centered(Rect::new(0, 0, 120, 30), 92, 64, 150, 12);
        let large = responsive_centered(Rect::new(0, 0, 240, 30), 92, 64, 150, 12);
        assert!(small.width < medium.width);
        assert!(medium.width < large.width);
        assert_eq!(large.width, 150);
    }

    #[test]
    fn apps_window_expands_with_the_terminal() {
        let small = apps_area(Rect::new(0, 0, 90, 30));
        let medium = apps_area(Rect::new(0, 0, 140, 30));
        let large = apps_area(Rect::new(0, 0, 220, 30));
        assert!(small.width < medium.width);
        assert!(medium.width < large.width);
        assert_eq!(large.width, 150);
    }

    #[test]
    fn partition_manager_renders_topology_details_and_safety_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            true,
        );
        let fixture = concat!(
            "PATH=\"/dev/sda\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" MODEL=\"Test Disk\" RO=\"0\" RM=\"0\" MAJ:MIN=\"8:0\"\n",
            "PATH=\"/dev/sda1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" LABEL=\"System\" UUID=\"test-uuid\" MOUNTPOINTS=\"/\" PKNAME=\"sda\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"0\" MAJ:MIN=\"8:1\"\n",
        );
        app.mode = AppMode::Partitions(PartitionView {
            entries: partition::from_lsblk_fixture(
                fixture,
                &[std::path::PathBuf::from("/dev/sda1")],
            )
            .unwrap()
            .entries,
            selected: 1,
            overlay: None,
        });
        let backend = TestBackend::new(140, 45);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Device manager"));
        assert!(rendered.contains("sda1"));
        assert!(rendered.contains("Protected system storage"));
        assert!(rendered.contains("UUID: test-uuid"));
        assert!(rendered.contains("Enter/a Actions"));

        if let AppMode::Partitions(view) = &mut app.mode {
            view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Choose what to do with /dev/sda1"));
        assert!(rendered.contains("Format"));
        assert!(rendered.contains("What it does"));
        assert!(rendered.contains("Erases data"));
        assert!(rendered.contains("Blocked"));
        assert!(rendered.contains("Enter Continue"));

        if let AppMode::Partitions(view) = &mut app.mode {
            view.selected = 0;
            view.overlay = Some(PartitionOverlay::Actions { selected: 1 });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Create partition"));
        assert!(rendered.contains("Format disk"));

        if let AppMode::Partitions(view) = &mut app.mode {
            view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected: 0 });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Free space"));
        assert!(rendered.contains("Region 1"));
        assert!(rendered.contains("Choose free space"));

        if let AppMode::Partitions(view) = &mut app.mode {
            view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                selected: 0,
                overwrite: false,
            });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Empty"));
        assert!(rendered.contains("GPT"));
        assert!(rendered.contains("MBR"));
        assert!(rendered.contains("Choose layout"));

        if let AppMode::Partitions(view) = &mut app.mode {
            view.selected = 1;
            view.overlay = Some(PartitionOverlay::FormatOptions {
                selected: 0,
                encrypted: false,
            });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Recommended Linux default"));
        assert!(rendered.contains("exFAT"));
        assert!(rendered.contains("Choose filesystem"));

        if let AppMode::Partitions(view) = &mut app.mode {
            let action = partition::PartitionAction::Format {
                target: partition::DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
                filesystem: partition::Filesystem::Ext4,
                label: None,
            };
            view.overlay = Some(PartitionOverlay::Confirm {
                action,
                yes_selected: false,
            });
        }
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("permanently erases data on the selected device"));
        assert!(rendered.contains("● No"));
        assert!(rendered.contains('═'));

        let view = match &app.mode {
            AppMode::Partitions(view) => view.clone(),
            _ => unreachable!(),
        };
        let action = match &view.overlay {
            Some(PartitionOverlay::Confirm { action, .. }) => action.clone(),
            _ => unreachable!(),
        };
        let mut input = crate::luks::SecretInput::default();
        for character in "not-a-real-password".chars() {
            input.push(character);
        }
        app.mode = AppMode::Prompt(Prompt::PartitionAuthentication {
            action,
            view,
            input,
            error: None,
        });
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Administrator authentication"));
        assert!(rendered.contains("••••"));
        assert!(!rendered.contains("not-a-real-password"));

        let view = match &app.mode {
            AppMode::Prompt(Prompt::PartitionAuthentication { view, .. }) => view.clone(),
            _ => unreachable!(),
        };
        app.mode = AppMode::Prompt(Prompt::PartitionError {
            body: "Action: Format\nDevice: /dev/sdb1\n\nReason:\nThe filesystem tool failed before formatting started."
                .into(),
            view,
        });
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Partition operation failed"));
        assert!(rendered.contains("Format"));
        assert!(rendered.contains("failed before formatting started"));
        assert!(rendered.contains("Enter/Esc return"));
    }

    #[test]
    #[ignore]
    fn benchmark_large_directory_render() {
        let path = std::env::var_os("MINFM_PERF_LARGE_DIR")
            .map(PathBuf::from)
            .expect("MINFM_PERF_LARGE_DIR is required");
        let app = App::new(
            path.clone(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: path.join("config.toml"),
            },
            true,
        );
        assert_eq!(app.entries.len(), 20_000);
        let backend = TestBackend::new(160, 50);
        let mut terminal = Terminal::new(backend).unwrap();
        let started = Instant::now();
        for _ in 0..100 {
            terminal.draw(|frame| draw(frame, &app)).unwrap();
        }
        let elapsed = started.elapsed();
        eprintln!(
            "PERF render_100_us={} render_mean_us={}",
            elapsed.as_micros(),
            elapsed.as_micros() / 100
        );
    }
}
