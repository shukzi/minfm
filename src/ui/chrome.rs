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

pub(super) fn header_action_line(app: &App, arrow: &str, compact: bool) -> Line<'static> {
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

pub(super) fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn draw_shortcuts(frame: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn shortcut_bar_height(app: &App, width: u16) -> u16 {
    let needs_second_line = matches!(app.mode, AppMode::Browser)
        && shortcut_items_width(&browser_shortcut_items(app)) > usize::from(width);
    if needs_second_line {
        2
    } else {
        1
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

pub(super) fn partition_shortcuts(app: &App, view: &crate::app::PartitionView) -> String {
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
