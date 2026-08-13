use super::*;
use super::{storage::draw_popup_halo, tools::append_trash_names};

pub(super) fn draw_prompt(frame: &mut Frame, app: &App, prompt: &Prompt) {
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

pub(super) fn draw_progress_modal(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_update_progress(frame: &mut Frame) {
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

pub(super) fn input_modal(frame: &mut Frame, title: &str, input: &str, footer: &str) {
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

pub(super) fn cursor_input_modal(
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

pub(super) fn cursor_input_modal_with_error(
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

pub(super) fn secret_input_modal(
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

pub(super) fn message_modal(
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

pub(super) fn smart_report_modal(frame: &mut Frame, body: &str, scroll: u16) {
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

pub(super) fn partition_error_modal(frame: &mut Frame, body: &str) {
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

pub(super) fn centered(area: Rect, width: u16, height: u16) -> Rect {
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

pub(super) fn responsive_centered(
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

pub(super) fn viewport_start(selected: usize, item_count: usize, visible_count: usize) -> usize {
    if item_count <= visible_count || selected < visible_count {
        0
    } else {
        (selected + 1 - visible_count).min(item_count - visible_count)
    }
}
