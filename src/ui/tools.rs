use super::*;
use super::{
    chrome::shortcut_bar_height,
    dialogs::{centered, message_modal, viewport_start},
};

pub(super) fn draw_archive(frame: &mut Frame, app: &App, view: &ArchiveView) {
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

pub(super) fn append_trash_names(body: &mut String, entries: &[crate::trash::TrashEntry]) {
    for entry in entries.iter().take(6) {
        body.push_str(&format!("{}\n", entry.display_name()));
    }
    if entries.len() > 6 {
        body.push_str(&format!("… and {} more\n", entries.len() - 6));
    }
}

pub(super) fn draw_trash(frame: &mut Frame, app: &App, view: &TrashView) {
    let screen = manager_content_area(app, frame.area());
    let area = Rect {
        x: screen.x.saturating_add(1),
        y: screen.y.saturating_add(1),
        width: screen.width.saturating_sub(2).max(1),
        height: screen.height.saturating_sub(2).max(1),
    };
    frame.render_widget(Clear, area);
    let visible_count = usize::from(area.height.saturating_sub(3)).max(1);
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
    frame.render_stateful_widget(table, area, &mut state);
}

pub(super) fn draw_network(frame: &mut Frame, app: &App, view: &NetworkView) {
    let area = manager_content_area(app, frame.area());
    frame.render_widget(Clear, area);
    let visible_count = usize::from(area.height.saturating_sub(3)).max(1);
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
    frame.render_stateful_widget(table, area, &mut state);
}

pub(super) fn draw_network_progress(frame: &mut Frame) {
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

pub(super) fn draw_help(frame: &mut Frame, app: &App) {
    let h = &app.config.hotkeys;
    let title = format!("Help · minfm {}", env!("CARGO_PKG_VERSION"));
    let area = centered(frame.area(), 84, 88);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    let left = help_column(&[
        (
            "Navigation",
            vec![
                (
                    format!("↑↓/{}/{}", h.down.display(), h.up.display()),
                    "Move",
                ),
                (format!("←/{}", h.collapse.display()), "Parent / collapse"),
                (format!("→/{}", h.expand.display()), "Open / expand"),
                (format!("Enter/{}", h.open.display()), "Open selected item"),
                (h.toggle_view.display().into(), "Toggle tree/table"),
            ],
        ),
        (
            "File actions",
            vec![
                (h.select.display().into(), "Mark"),
                (h.cut.display().into(), "Cut"),
                (h.copy.display().into(), "Copy"),
                (h.paste.display().into(), "Paste"),
                (h.rename.display().into(), "Rename"),
                (h.edit.display().into(), "Edit text file"),
                (h.archive.display().into(), "Archive actions"),
            ],
        ),
        (
            "Trash",
            vec![
                (h.trash.display().into(), "Move to Trash"),
                (h.quick_trash.display().into(), "Quick Trash"),
                (h.trash_bin.display().into(), "Open Trash"),
            ],
        ),
    ]);
    let right = help_column(&[
        (
            "Find and create",
            vec![
                (h.go_to.display().into(), "Go to path"),
                (h.search.display().into(), "Search directory"),
                (h.search_filesystem.display().into(), "Search filesystem"),
                (h.create_file.display().into(), "Create file"),
                (h.create_directory.display().into(), "Create directory"),
            ],
        ),
        (
            "View and tools",
            vec![
                (h.hidden.display().into(), "Toggle hidden files"),
                (h.sort.display().into(), "Change sort mode"),
                (h.reverse_sort.display().into(), "Reverse sort"),
                (h.tools.display().into(), "Open tools"),
                (h.network_shares.display().into(), "Open shares"),
                (h.devices.display().into(), "Open devices"),
                (h.info.display().into(), "Information"),
            ],
        ),
        (
            "Application",
            vec![
                (h.help.display().into(), "Help"),
                (h.quit.display().into(), "Quit"),
                (h.force_quit.display().into(), "Force quit"),
            ],
        ),
    ]);

    if rows[0].width >= 50 {
        let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        frame.render_widget(Paragraph::new(left), columns[0]);
        frame.render_widget(Paragraph::new(right), columns[1]);
    } else {
        let mut combined = left;
        combined.lines.extend(right.lines);
        frame.render_widget(Paragraph::new(combined), rows[0]);
    }
    frame.render_widget(
        Paragraph::new("Enter: close · Esc: close")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
}

fn help_column(sections: &[(&str, Vec<(String, &str)>)]) -> Text<'static> {
    let mut lines = Vec::new();
    for (title, items) in sections {
        lines.push(Line::from(Span::styled(
            (*title).to_owned(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        for (key, label) in items {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<11}"),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled((*label).to_owned(), Style::default().fg(MUTED)),
            ]));
        }
    }
    Text::from(lines)
}

pub(super) fn draw_info(frame: &mut Frame, app: &App, entry: Option<&crate::entry::FileEntry>) {
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let focused = entry.map_or_else(String::new, |entry| {
        format!(
            "Selected item:\nPath: {}\nKind: {:?}\nSize: {}\nPermissions: {}\nModified: {}\n\n",
            entry.path.display(),
            entry.kind,
            entry.size_text(),
            entry.permissions(),
            entry.modified_text()
        )
    });
    let body = format!(
        "{}minfm {}\n\nBinary:\n{}\n\nConfig:\n{}\n\nCurrent directory:\n{}\n\nMode: {}\nView: {}\nSort: {} {}\n\nSystem tools:\n  lsblk: {}\n  udisksctl: {}\n  cryptsetup: {}\n  smartctl: {}\n  hdparm: {}\n  gio: {}\n  secret-tool: {}\n  parted: {}\n  wipefs: {}\n  sfdisk: {}\n  sudo: {}",
        focused,
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
        "Enter: close · Esc: close",
        78,
        32,
    );
}

pub(super) fn availability(command: &str) -> &'static str {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
        })
        .filter(|available| *available)
        .map(|_| "available")
        .unwrap_or("missing")
}

pub(super) fn draw_config_error(frame: &mut Frame, app: &App, path: &std::path::Path, error: &str) {
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
