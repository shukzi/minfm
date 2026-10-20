use super::dialogs::{centered, responsive_centered, viewport_start};
use super::*;

pub(super) fn draw_tools(frame: &mut Frame, app: &App, view: &ToolsView) {
    let area = tools_area(manager_content_area(app, frame.area()));
    frame.render_widget(Clear, area);
    let rows = BuiltinTool::ALL.into_iter().map(|builtin| {
        let status = match builtin {
            BuiltinTool::DeviceManager
                if app.config.behavior.read_only || !app.device_manager_available() =>
            {
                "Unavailable"
            }
            BuiltinTool::NetworkShares if !app.network_shares_available() => "Unavailable",
            _ => "Available",
        };
        Row::new([builtin.name(), builtin.description(), status])
    });
    let table = Table::new(rows, tools_table_widths(area.width))
        .header(Row::new(["Tool", "Purpose", "State"]).style(Style::default().fg(MUTED)))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ")
        .block(Block::default().borders(Borders::ALL).title(" Tools "));
    let mut state = TableState::default().with_selected(Some(view.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

pub(super) fn tools_area(area: Rect) -> Rect {
    responsive_centered(area, 88, 68, 150, 12)
}

pub(super) fn draw_partitions(frame: &mut Frame, app: &App, view: &PartitionView) {
    let area = responsive_centered(manager_content_area(app, frame.area()), 96, 72, 150, 92);
    frame.render_widget(Clear, area);
    let sections = Layout::vertical([Constraint::Min(8), Constraint::Length(10)]).split(area);
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
    if let Some(overlay) = &view.overlay {
        draw_partition_overlay(frame, app, view, overlay);
    }
}

pub(super) fn draw_partition_overlay(
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
                    "Enter: continue · {}/Esc: back",
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
            permissions,
        } => {
            let target = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.display().to_string())
                .unwrap_or_else(|| "selected device".into());
            draw_format_options(frame, &target, *selected, *encrypted, *permissions);
        }
        PartitionOverlay::EncryptionFilesystem {
            selected,
            whole_disk,
            ..
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
            "Enter: review · Esc: free regions",
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
            "Enter: review · Esc: filesystems",
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
            "Enter: review · Esc: actions",
        ),
        PartitionOverlay::Confirm {
            action,
            yes_selected,
        } => draw_partition_confirmation(frame, action, *yes_selected),
    }
}

pub(super) fn draw_format_options(
    frame: &mut Frame,
    target: &str,
    selected: usize,
    encrypted: bool,
    permissions: crate::partition::FilesystemPermissions,
) {
    let area = responsive_centered(frame.area(), 80, 60, 110, 20);
    draw_popup_halo(frame, area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(3),
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
        Paragraph::new(if Filesystem::ALL[selected].supports_unix_ownership() {
            format!(
                "[{}] Allow everyone to read and write",
                if permissions == crate::partition::FilesystemPermissions::Everyone {
                    "x"
                } else {
                    " "
                }
            )
        } else {
            "Permissions are set by the computer that mounts this filesystem".into()
        })
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED))
        .block(Block::default().borders(Borders::ALL)),
        sections[2],
    );
    frame.render_widget(
        Paragraph::new("p: permissions · Enter: continue · Esc: back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[3],
    );
}

pub(super) fn draw_popup_halo(frame: &mut Frame, area: Rect) {
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

pub(super) fn draw_encryption_filesystems(
    frame: &mut Frame,
    target: &str,
    selected: usize,
    whole_disk: bool,
) {
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
            "Choose the filesystem stored inside encryption · Enter: continue · Esc: back",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

pub(super) fn draw_encryption_passphrase(
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
        Paragraph::new("Enter: continue · Esc: back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[4],
    );
}

pub(super) fn draw_change_passphrase(
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
        Paragraph::new("Enter: continue · Esc: back").alignment(Alignment::Center),
        rows[5],
    );
}

pub(super) fn draw_disk_layout_options(
    frame: &mut Frame,
    target: &str,
    selected: usize,
    overwrite: bool,
) {
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
            "w Full overwrite: {} · Enter: review · Esc: back",
            if overwrite { "on" } else { "off" }
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(ACCENT))
        .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

pub(super) fn draw_free_region_options(
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
        Paragraph::new("Choose free space · Enter: continue · Esc: back")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT))
            .block(Block::default().borders(Borders::ALL)),
        sections[1],
    );
}

pub(super) fn draw_partition_input(
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

pub(super) fn draw_partition_confirmation(
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
        Paragraph::new("←/→: choose · Enter: apply · Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        rows[4],
    );
}

pub(super) fn partition_button_width(area_width: u16) -> u16 {
    area_width.saturating_sub(6).saturating_div(2).clamp(8, 16)
}

pub(super) fn tools_table_widths(width: u16) -> Vec<Constraint> {
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

pub(super) fn partition_table_widths(width: u16) -> Vec<Constraint> {
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

pub(super) fn partition_action_widths(width: u16) -> Vec<Constraint> {
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

pub(super) fn draw_partition_edit_field(frame: &mut Frame, area: Rect, input: &str, cursor: usize) {
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

pub(super) fn partition_details(
    entry: &crate::partition::PartitionEntry,
    view: &PartitionView,
) -> String {
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

pub(super) fn partition_status(entry: &crate::partition::PartitionEntry) -> &'static str {
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

pub(super) fn display_or_dash(value: Option<&str>) -> &str {
    value.filter(|value| !value.is_empty()).unwrap_or("—")
}

pub(super) fn draw_devices(frame: &mut Frame, app: &App, view: &DeviceView) {
    let area = centered(manager_content_area(app, frame.area()), 92, 80);
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
}
