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

pub(super) fn draw_network(frame: &mut Frame, app: &App, view: &NetworkView) {
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
        "Esc/Enter close",
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
