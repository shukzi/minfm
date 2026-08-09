use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

use crate::{
    app::{
        format_elapsed, App, AppMode, BrowserView, ClipboardMode, DeviceView, NetworkView, Prompt,
        SearchScope, SearchView, TrashView,
    },
    entry::{human_size, EntryKind},
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

pub fn draw(frame: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, app, rows[0]);
    draw_browser(frame, app, rows[1]);
    draw_status(frame, app, rows[2]);
    draw_shortcuts(frame, app, rows[3]);

    match &app.mode {
        AppMode::Browser => {}
        AppMode::Prompt(prompt) => draw_prompt(frame, prompt),
        AppMode::Progress => draw_progress_modal(frame, app),
        AppMode::SearchProgress => draw_search_progress(frame, app),
        AppMode::SearchResults(view) => draw_search_results(frame, view),
        AppMode::UpdateProgress => draw_update_progress(frame),
        AppMode::Trash(view) => draw_trash(frame, view),
        AppMode::Devices(view) => draw_devices(frame, app, view),
        AppMode::Network(view) => draw_network(frame, app, view),
        AppMode::NetworkProgress => draw_network_progress(frame),
        AppMode::Help => draw_help(frame),
        AppMode::Info => draw_info(frame, app),
        AppMode::ConfigError { path, error } => draw_config_error(frame, path, error),
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(" {} ", app.current_dir.display());
    let mode = if app.config.behavior.read_only {
        "READ ONLY"
    } else {
        "minfm"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(mode, Style::default().fg(MUTED)),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" minfm ")),
        area,
    );
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
    let search_query = app.search_filter.as_deref();
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
            let branch = if entry.kind == EntryKind::Directory {
                if app.is_tree_directory_expanded(&entry.path) {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            let suffix = if entry.kind == EntryKind::Directory {
                "/"
            } else {
                ""
            };
            let name = format!("{}{branch}{}{suffix}", "  ".repeat(depth), entry.name);
            let name_cell = if search_query.is_some() {
                Cell::from(Span::styled(
                    name,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
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

fn draw_file_table(frame: &mut Frame, app: &App, area: Rect) {
    let search_query = app.search_filter.as_deref();
    let visible_count = usize::from(area.height.saturating_sub(3)).max(1);
    let start = viewport_start(app.cursor, app.entries.len(), visible_count);
    let end = (start + visible_count).min(app.entries.len());
    let rows = app.entries[start..end].iter().map(|entry| {
        let marker = if entry.selected { "●" } else { " " };
        let suffix = if entry.kind == EntryKind::Directory {
            "/"
        } else {
            ""
        };
        let name = format!("{}{suffix}", entry.name);
        let name_cell = if search_query.is_some() {
            Cell::from(Span::styled(
                name,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))
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
        app.search_filter
            .as_deref()
            .map(|query| format!(" · search: {query}"))
            .unwrap_or_default(),
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
    if !matches!(app.mode, AppMode::Browser) {
        frame.render_widget(
            Paragraph::new(" A dialog owns input. File shortcuts are disabled until it closes. ")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray)),
            area,
        );
        return;
    }

    let edit = if app
        .selected_entry()
        .is_some_and(|entry| entry.is_text_file())
    {
        " · e Edit"
    } else {
        ""
    };
    let open_controls = match app.browser_view {
        BrowserView::Tree => "→l Expand · Enter Open",
        BrowserView::Table => "→l/Enter Open",
    };
    let view = match app.browser_view {
        BrowserView::Tree => "v Table",
        BrowserView::Table => "v Tree",
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(area);
    let groups = [
        format!(
            " NAVIGATION\n ↑↓/jk Move · ←h Parent\n {open_controls}{edit} · {view} · ? Help"
        ),
        " FILE OPERATIONS\n Space Mark · x Cut · c Copy · p Paste\n r Rename · n File · a Dir".into(),
        " VIEW & DEVICES\n d/D Trash · T Bin · . Hidden · s Sort\n m Devices · N Shares · g Path · / Search · F Search FS".into(),
    ];

    for (index, group) in groups.into_iter().enumerate() {
        let block = if index < columns.len() - 1 {
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(Color::DarkGray))
        } else {
            Block::default()
        };
        frame.render_widget(
            Paragraph::new(group)
                .block(block)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray)),
            columns[index],
        );
    }
}

fn draw_prompt(frame: &mut Frame, prompt: &Prompt) {
    match prompt {
        Prompt::GoTo { input } => input_modal(frame, "Go to path", input, "Enter go · Esc cancel"),
        Prompt::Search { input, scope } => input_modal(
            frame,
            match scope {
                SearchScope::CurrentDirectory => "Search current directory",
                SearchScope::Filesystem => "Search entire filesystem",
            },
            input,
            "Enter search · Esc cancel",
        ),
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
                "Enter confirm · Esc cancel",
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
                "o/Enter overwrite · s skip conflicts · a/Esc abort",
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
                "r/Enter restore · Esc cancel",
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
                "d/Enter permanently delete · Esc cancel",
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
            frame.render_widget(Clear, area);
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
        Prompt::SmbRemember { available, .. } => message_modal(
            frame,
            "Remember this share?",
            if *available {
                "Save the password in your desktop's secure credential service?\n\nThe share address and account name will be saved in minfm's configuration directory."
            } else {
                "Secure credential storage is unavailable.\n\nThe connection will only last for this login session."
            },
            if *available {
                "y remember · n/Enter this session only · Esc cancel"
            } else {
                "Enter this session only · Esc cancel"
            },
            76,
            14,
        ),
        Prompt::ConfirmSmbDisconnect { share } => message_modal(
            frame,
            "Disconnect network share",
            &format!("Disconnect {}?\n\nOpen files on this share may prevent disconnection.", share.address.uri),
            "u/Enter disconnect · Esc cancel",
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
            "d/Enter forget · Esc cancel",
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Search entire filesystem ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let message = if app.search_cancelling {
        "Cancellation requested…"
    } else {
        "Searching from / …"
    };
    let body = format!(
        "{}\n\nMatches found: {}\nPermission errors skipped: {}\n\nEsc cancel",
        message, app.search_matches, app.search_skipped
    );
    frame.render_widget(
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        inner,
    );
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

fn draw_search_results(frame: &mut Frame, view: &SearchView) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let visible_count = usize::from(area.height.saturating_sub(4)).max(1);
    let start = viewport_start(view.selected, view.results.len(), visible_count);
    let end = (start + visible_count).min(view.results.len());
    let rows = view
        .results
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|path| Row::new([Cell::from(path.display().to_string())]));
    let table = Table::new(rows, [Constraint::Min(20)])
        .header(
            Row::new([format!("{} · {} result(s)", view.query, view.results.len())])
                .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("> ")
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Filesystem search "),
        );
    let selected = (!view.results.is_empty()).then_some(view.selected.saturating_sub(start));
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
    let footer = if view.limited {
        "10,000 result limit reached · ↑/↓ j/k Move · Enter open · Esc return".to_string()
    } else if view.skipped == 0 {
        "↑/↓ j/k Move · Enter open · / Search here · F Search filesystem · Esc return".to_string()
    } else {
        format!(
            "↑/↓ j/k Move · Enter open · Esc return · {} permission error(s) skipped",
            view.skipped
        )
    };
    let footer_area = Rect {
        x: area.x + 1,
        y: area.bottom().saturating_sub(2),
        width: area.width.saturating_sub(2),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().fg(MUTED)),
        footer_area,
    );
}

fn append_trash_names(body: &mut String, entries: &[crate::trash::TrashEntry]) {
    for entry in entries.iter().take(6) {
        body.push_str(&format!("{}\n", entry.display_name()));
    }
    if entries.len() > 6 {
        body.push_str(&format!("… and {} more\n", entries.len() - 6));
    }
}

fn draw_trash(frame: &mut Frame, view: &TrashView) {
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
        Paragraph::new(
            "Space select │ Enter/r restore │ d permanent delete │ D quick permanent delete\nC clear trash │ T/Esc return",
        )
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

fn draw_devices(frame: &mut Frame, app: &App, view: &DeviceView) {
    let area = centered(frame.area(), 92, 80);
    frame.render_widget(Clear, area);
    let rows = view.devices.iter().map(|device| {
        Row::new(vec![
            Cell::from(device.source.display().to_string()),
            Cell::from(device.label.clone().unwrap_or_else(|| "—".into())),
            Cell::from(human_size(device.size)),
            Cell::from(device.state_text()),
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
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Encrypted devices · {} found ",
        view.devices.len()
    )));
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
            let mut action = if device.is_locked() {
                "Enter/u unlock and mount".to_string()
            } else if device.is_mounted() {
                "Enter/u unmount and lock".to_string()
            } else {
                "Enter/m mount".to_string()
            };
            if device.ejectable && !device.eject_blocked {
                action.push_str(" · e eject");
            } else if device.ejectable && device.eject_blocked {
                action.push_str(" · eject unavailable: drive in use");
            }
            action
        })
        .unwrap_or_else(|| {
            if app.device_refreshing {
                "Refreshing devices…".into()
            } else {
                "No encrypted volumes found".into()
            }
        });
    frame.render_widget(
        Paragraph::new(format!("{action} · r refresh · Esc return"))
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
                "Enter open · u disconnect".to_string()
            } else {
                "Enter connect".to_string()
            };
            if share.saved {
                actions.push_str(" · d forget");
            }
            actions
        })
        .unwrap_or_else(|| "No shares found · use a to add one".into());
    frame.render_widget(
        Paragraph::new(format!(
            "{contextual}\na add share · r refresh · N/Esc return"
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

fn draw_help(frame: &mut Frame) {
    let body = "Navigation\n  ↑/k ↓/j       move\n  Enter          toggle directory or open file\n  →/l            expand/child in tree; open in table\n  ←/h            collapse/parent in tree; parent in table\n  v              toggle tree/table view\n  g              go to path\n  /              search current directory\n  F              search entire filesystem\n\nClipboard\n  x              cut\n  c              copy\n  p              paste\n\nFiles\n  Space          select\n  e              edit selected text file\n  r              rename file or directory\n  d / D          trash with prompt / quick trash\n  T              trash bin\n\nTrash bin\n  Space          select\n  Enter / r      restore\n  d / D          permanent delete / quick permanent delete\n  C              clear trash with confirmation\n\nCreate\n  n              create file\n  a              create directory\n\nDevices\n  m              device manager\n  e              safely eject selected removable device\n\nNetwork shares\n  N              network shares\n  a              add share\n  Enter          open or connect\n  u              disconnect\n  d              forget saved share\n  r              refresh\n\nView\n  .              hidden files\n  s / S          sort mode / reverse\n  I              information\n  Esc            close this view";
    let title = format!("Help · minfm {}", env!("CARGO_PKG_VERSION"));
    message_modal(frame, &title, body, "Esc/Enter close", 70, 94);
}

fn draw_info(frame: &mut Frame, app: &App) {
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let body = format!(
        "minfm {}\n\nBinary:\n{}\n\nConfig:\n{}\n\nCurrent directory:\n{}\n\nMode: {}\nView: {}\nSort: {} {}\n\nSystem tools:\n  lsblk: {}\n  udisksctl: {}\n  cryptsetup: {}\n  gio: {}\n  secret-tool: {}",
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
        availability("gio"),
        availability("secret-tool"),
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

fn draw_config_error(frame: &mut Frame, path: &std::path::Path, error: &str) {
    let body = format!(
        "minfm cannot use its configuration.\n\nFile:\n{}\n\n{}\n\nAll file interaction is disabled until the configuration is corrected and reloaded.",
        path.display(), error
    );
    message_modal(
        frame,
        "Configuration error",
        &body,
        "e edit config · r reload · q quit",
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
    use crate::config::{Config, ConfigLoad};
    use crate::network::{NetworkSecret, ShareAddress};
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
