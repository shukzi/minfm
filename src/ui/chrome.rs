use super::*;

pub(super) fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
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
        "{} Open Trash   {} Info   {} Devices   Sort: {} {}",
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

pub(super) fn header_action_line(app: &App, arrow: &str, compact: bool) -> Line<'static> {
    let key_style = Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(MUTED);
    let mut spans = Vec::new();
    for (key, label) in [
        (app.config.hotkeys.trash_bin.display(), "Open Trash"),
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

pub(super) fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    if matches!(
        app.mode,
        AppMode::Tools(_)
            | AppMode::Trash(_)
            | AppMode::Devices(_)
            | AppMode::Network(_)
            | AppMode::Partitions(_)
    ) {
        return;
    }
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

pub(super) fn status_bar_height(app: &App) -> u16 {
    if matches!(
        app.mode,
        AppMode::Tools(_)
            | AppMode::Trash(_)
            | AppMode::Devices(_)
            | AppMode::Network(_)
            | AppMode::Partitions(_)
    ) {
        0
    } else {
        1
    }
}

pub(super) fn draw_shortcuts(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(items) = shortcut_items(app) {
        let lines = shortcut_lines_owned(&items, area.width, usize::from(area.height));
        frame.render_widget(
            Paragraph::new(Text::from(lines)).style(Style::default().fg(MUTED)),
            area,
        );
    }
}

pub(super) fn shortcut_items(app: &App) -> Option<Vec<(String, &'static str)>> {
    match &app.mode {
        AppMode::Browser => Some(browser_shortcut_items(app)),
        AppMode::Tools(_) => Some(tools_shortcut_items(app)),
        AppMode::Archive(_) => Some(vec![
            (move_key(app), "Move"),
            (
                format!("{}/Esc", app.config.hotkeys.quit.display()),
                "Files",
            ),
        ]),
        AppMode::Trash(_) => Some(trash_shortcut_items(app)),
        AppMode::Devices(view) => Some(device_shortcut_items(app, view)),
        AppMode::Network(view) => Some(network_shortcut_items(app, view)),
        AppMode::Partitions(view) if view.overlay.is_none() => Some(partition_shortcut_items(app)),
        _ => None,
    }
}

fn move_key(app: &App) -> String {
    format!(
        "↑↓/{}/{}",
        app.config.hotkeys.down.display(),
        app.config.hotkeys.up.display()
    )
}

pub(super) fn tools_shortcut_items(app: &App) -> Vec<(String, &'static str)> {
    vec![
        (move_key(app), "Move"),
        ("Enter".into(), "Open"),
        (
            format!(
                "{}/{}/Esc",
                app.config.hotkeys.tools.display(),
                app.config.hotkeys.quit.display()
            ),
            "Files",
        ),
    ]
}

pub(super) fn trash_shortcut_items(app: &App) -> Vec<(String, &'static str)> {
    vec![
        (move_key(app), "Move"),
        (app.config.hotkeys.select.display().into(), "Mark"),
        (
            format!("Enter/{}", app.config.hotkeys.restore.display()),
            "Restore",
        ),
        (
            app.config.hotkeys.permanent_delete.display().into(),
            "Delete permanently",
        ),
        (
            app.config.hotkeys.quick_permanent_delete.display().into(),
            "Quick delete",
        ),
        (
            app.config.hotkeys.clear_trash.display().into(),
            "Empty Trash",
        ),
        (
            format!("{}/Esc", app.config.hotkeys.trash_bin.display()),
            "Files",
        ),
    ]
}

pub(super) fn manager_exit_shortcuts(app: &App) -> Vec<(String, &'static str)> {
    let mut items = Vec::new();
    if app.manager_returns_to_tools() {
        items.push((
            format!("Esc/{}", app.config.hotkeys.tools.display()),
            "Tools",
        ));
        items.push((app.config.hotkeys.quit.display().into(), "Files"));
    } else {
        items.push((app.config.hotkeys.tools.display().into(), "Tools"));
        items.push((
            format!("Esc/{}", app.config.hotkeys.quit.display()),
            "Files",
        ));
    }
    items
}

pub(super) fn partition_shortcut_items(app: &App) -> Vec<(String, &'static str)> {
    let mut items = vec![
        (move_key(app), "Move"),
        (
            format!("Enter/{}", app.config.hotkeys.partition_actions.display()),
            "Actions",
        ),
        (app.config.hotkeys.refresh.display().into(), "Refresh"),
    ];
    items.extend(manager_exit_shortcuts(app));
    items
}

pub(super) fn device_shortcut_items(
    app: &App,
    view: &crate::app::DeviceView,
) -> Vec<(String, &'static str)> {
    let mut items = vec![(move_key(app), "Move")];
    if let Some(device) = view.devices.get(view.selected) {
        if !device.system_protected && (device.encrypted || device.filesystem.is_some()) {
            let label = if device.encrypted && device.is_locked() {
                "Unlock"
            } else if device.is_mounted() {
                "Unmount"
            } else {
                "Mount"
            };
            items.push(("Enter".into(), label));
        }
        if device.ejectable && !device.eject_blocked && !device.system_protected {
            items.push((app.config.hotkeys.device_eject.display().into(), "Eject"));
        }
    }
    items.push((app.config.hotkeys.refresh.display().into(), "Refresh"));
    items.extend(manager_exit_shortcuts(app));
    items
}

pub(super) fn browser_shortcut_items(app: &App) -> Vec<(String, &'static str)> {
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

pub(super) fn network_shortcut_items(
    app: &App,
    view: &crate::app::NetworkView,
) -> Vec<(String, &'static str)> {
    let mut items = vec![
        (move_key(app), "Move"),
        ("Enter".into(), "Open/Connect"),
        (app.config.hotkeys.network_add.display().into(), "Add"),
        (app.config.hotkeys.refresh.display().into(), "Refresh"),
    ];
    if let Some(share) = view.shares.get(view.selected) {
        if share.mount_path.is_some() {
            items.push((
                app.config.hotkeys.network_disconnect.display().into(),
                "Disconnect",
            ));
        }
        if share.saved {
            items.push((app.config.hotkeys.network_forget.display().into(), "Forget"));
        }
    }
    items.extend(manager_exit_shortcuts(app));
    items
}

pub(super) fn shortcut_bar_height(app: &App, width: u16) -> u16 {
    let Some(items) = shortcut_items(app) else {
        return 0;
    };
    if shortcut_items_width(&items) > usize::from(width) {
        2
    } else {
        1
    }
}

pub(super) fn manager_content_area(app: &App, screen: Rect) -> Rect {
    Rect {
        height: screen
            .height
            .saturating_sub(shortcut_bar_height(app, screen.width)),
        ..screen
    }
}

pub(super) fn shortcut_items_width(items: &[(String, &'static str)]) -> usize {
    1 + items
        .iter()
        .map(|(key, label)| UnicodeWidthStr::width(key.as_str()) + 1 + label.len())
        .sum::<usize>()
        + items.len().saturating_sub(1) * 3
}

pub(super) fn shortcut_lines_owned(
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
