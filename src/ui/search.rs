use super::*;
use super::{
    chrome::shortcut_bar_height,
    dialogs::{centered, viewport_start},
};

pub(super) fn draw_search_progress(frame: &mut Frame, app: &App) {
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
            "Esc: cancel"
        }
    );
    frame.render_widget(
        Paragraph::new(body)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ACCENT)),
        inner,
    );
}

pub(super) fn draw_search_form(frame: &mut Frame, app: &App, form: &SearchForm) {
    if form.advanced {
        draw_advanced_search(frame, app, form);
        return;
    }
    draw_quick_search(frame, app, form);
}

pub(super) fn draw_quick_search(frame: &mut Frame, app: &App, form: &SearchForm) {
    let area = centered(frame.area(), 72, if form.error.is_some() { 10 } else { 9 });
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Search current directory ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical(if form.error.is_some() {
        vec![
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ]
    })
    .split(inner);
    frame.render_widget(Paragraph::new("Enter a value:"), rows[0]);
    let query = cursor_window(
        &form.draft.name,
        form.cursors.name,
        "<name or pattern>",
        usize::from(rows[1].width.saturating_sub(6)),
    );
    frame.render_widget(
        Paragraph::new(format!("> {query}"))
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    let footer_row = if let Some(error) = &form.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)),
            rows[2],
        );
        rows[3]
    } else {
        rows[2]
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Enter: search · {} advanced · Esc: cancel",
            app.config.hotkeys.search_filesystem.display()
        ))
        .alignment(Alignment::Center)
        .style(Style::default().fg(MUTED)),
        footer_row,
    );
}

pub(super) fn draw_advanced_search(frame: &mut Frame, app: &App, form: &SearchForm) {
    let screen = frame.area();
    let area = centered(
        screen,
        screen.width.saturating_sub(4).min(110),
        screen.height.saturating_sub(4).min(32),
    );
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
            cursor_text(
                &form.draft.name,
                form.cursors.name,
                "<name or pattern>",
                usize::from(rows[0].width.saturating_sub(9)),
            )
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
    let content_area = columns[1].inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    let help = search_help_text(form);
    let help_inner_width = content_area.width.saturating_sub(2);
    let wrapped_help = hard_wrap_text(help, usize::from(help_inner_width));
    let wrapped_help_rows = wrapped_help.lines().count().min(u16::MAX as usize) as u16;
    let desired_help_height = wrapped_help_rows.saturating_add(2);
    let help_height = if content_area.width >= 3 && content_area.height >= 6 {
        desired_help_height.min(content_area.height.saturating_sub(1))
    } else {
        0
    };
    let content_rows =
        Layout::vertical([Constraint::Min(1), Constraint::Length(help_height)]).split(content_area);
    let control_area = content_rows[0];
    let (controls, active_line) = if control_area.height <= 1 {
        (
            active_search_control_text(form, usize::from(control_area.width)),
            0,
        )
    } else {
        search_control_text(form, usize::from(control_area.width))
    };
    let wrapped_controls = hard_wrap_text(&controls, usize::from(control_area.width));
    let rendered_through_active = if control_area.width == 0 {
        0
    } else {
        hard_wrap_text(
            &controls
                .lines()
                .take(active_line.saturating_add(1))
                .collect::<Vec<_>>()
                .join("\n"),
            usize::from(control_area.width),
        )
        .lines()
        .count()
    };
    let scroll = rendered_through_active.saturating_sub(usize::from(control_area.height));
    frame.render_widget(
        Paragraph::new(wrapped_controls).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        control_area,
    );
    if help_height >= 3 {
        frame.render_widget(
            Paragraph::new(wrapped_help)
                .block(Block::default().borders(Borders::ALL).title(" Help ")),
            content_rows[1],
        );
    }
    if let Some(error) = &form.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(ACCENT)),
            rows[2],
        );
    }
    frame.render_widget(
        Paragraph::new(if area.width < 90 {
            format!(
                "Tab: field · Enter: search · Esc: {}",
                match form.return_to {
                    crate::app::SearchReturn::Browser => "cancel",
                    crate::app::SearchReturn::Results => "results",
                }
            )
        } else {
            format!(
                "↑/↓: section · Tab: field · ←/→: choice · Space: toggle · Enter: search · Esc: {}",
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

pub(super) fn hard_wrap_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut output = String::new();
    for (line_index, line) in text.split('\n').enumerate() {
        if line_index > 0 {
            output.push('\n');
        }
        let mut line_width: usize = 0;
        for grapheme in UnicodeSegmentation::graphemes(line, true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if line_width > 0 && line_width.saturating_add(grapheme_width) > width {
                output.push('\n');
                line_width = 0;
            }
            output.push_str(grapheme);
            line_width = line_width.saturating_add(grapheme_width);
        }
    }
    output
}

pub(super) fn active_search_control_text(form: &SearchForm, width: usize) -> String {
    use crate::search::{ContentMode, NameMode, ResultLimit, SearchScope};

    pub(super) fn clipped(text: &str, width: usize) -> String {
        let mut output = String::new();
        for grapheme in text.graphemes(true) {
            if UnicodeWidthStr::width(output.as_str()) + UnicodeWidthStr::width(grapheme) > width {
                break;
            }
            output.push_str(grapheme);
        }
        output
    }

    pub(super) fn prefix_for<'a>(
        full: &'a str,
        compact: &'a str,
        width: usize,
        value_budget: usize,
    ) -> &'a str {
        if UnicodeWidthStr::width(full).saturating_add(value_budget) <= width {
            full
        } else {
            compact
        }
    }

    pub(super) fn choice(
        full: &str,
        compact: &str,
        value: &str,
        width: usize,
        value_budget: usize,
    ) -> String {
        let prefix = prefix_for(full, compact, width, value_budget);
        let prefix = clipped(prefix, width);
        let remaining = width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
        format!("{prefix}{}", clipped(value, remaining))
    }

    pub(super) fn text(
        full: &str,
        compact: &str,
        value: &str,
        cursor: usize,
        hint: &str,
        width: usize,
    ) -> String {
        let prefix = prefix_for(full, compact, width, 1);
        let prefix = clipped(prefix, width);
        let remaining = width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
        format!("{prefix}{}", cursor_text(value, cursor, hint, remaining))
    }

    match (form.section, form.field) {
        (crate::app::SearchSection::Scope, _) => choice(
            "> Scope: ",
            "> Scope: ",
            match form.draft.scope {
                SearchScope::CurrentDirectory => "Current directory",
                SearchScope::RecursiveHere => "Recursive here",
                SearchScope::Filesystem => "Entire filesystem",
            },
            width,
            6,
        ),
        (crate::app::SearchSection::Match, 0) => choice(
            "> Name mode: ",
            "> Name: ",
            match form.draft.name_mode {
                NameMode::Smart => "Smart",
                NameMode::Glob => "Glob",
                NameMode::Regex => "Regex",
            },
            width,
            5,
        ),
        (crate::app::SearchSection::Match, 1) => text(
            "> Content: ",
            "> Content: ",
            &form.draft.content,
            form.cursors.content,
            "<optional>",
            width,
        ),
        (crate::app::SearchSection::Match, _) => choice(
            "> Content mode: ",
            "> Content md: ",
            match form.draft.content_mode {
                ContentMode::Literal => "Literal",
                ContentMode::Regex => "Regex",
            },
            width,
            2,
        ),
        (crate::app::SearchSection::Filters, 0) => choice(
            "> ",
            "> ",
            &format!(
                "{} Files",
                selected(form.draft.types.contains(EntryKind::File))
            ),
            width,
            1,
        ),
        (crate::app::SearchSection::Filters, 1) => choice(
            "> ",
            "> ",
            &format!(
                "{} Directories",
                selected(form.draft.types.contains(EntryKind::Directory))
            ),
            width,
            1,
        ),
        (crate::app::SearchSection::Filters, 2) => choice(
            "> ",
            "> ",
            &format!(
                "{} Symlinks",
                selected(form.draft.types.contains(EntryKind::Symlink))
            ),
            width,
            1,
        ),
        (crate::app::SearchSection::Filters, 3) => choice(
            "> ",
            "> ",
            &format!(
                "{} Block devices",
                selected(form.draft.types.contains(EntryKind::BlockDevice))
            ),
            width,
            1,
        ),
        (crate::app::SearchSection::Filters, 4) => choice(
            "> ",
            "> ",
            &format!(
                "{} Other",
                selected(form.draft.types.contains(EntryKind::Other))
            ),
            width,
            1,
        ),
        (crate::app::SearchSection::Filters, 5) => text(
            "> Minimum size: ",
            "> Minimum: ",
            &form.draft.minimum_size,
            form.cursors.minimum_size,
            "—",
            width,
        ),
        (crate::app::SearchSection::Filters, 6) => text(
            "> Maximum size: ",
            "> Maximum: ",
            &form.draft.maximum_size,
            form.cursors.maximum_size,
            "—",
            width,
        ),
        (crate::app::SearchSection::Filters, 7) => text(
            "> Modified after: ",
            "> After: ",
            &form.draft.modified_after,
            form.cursors.modified_after,
            "—",
            width,
        ),
        (crate::app::SearchSection::Filters, 8) => text(
            "> Modified before: ",
            "> Before: ",
            &form.draft.modified_before,
            form.cursors.modified_before,
            "—",
            width,
        ),
        (crate::app::SearchSection::Filters, _) => {
            let value = if form.draft.include_ignored_hidden {
                "Yes"
            } else {
                "No"
            };
            choice("> Include ignored/hidden: ", "> Include: ", value, width, 3)
        }
        (crate::app::SearchSection::Traversal, _) => choice(
            "> Result limit: ",
            "> Limit: ",
            match form.draft.result_limit {
                ResultLimit::OneThousand => "1,000",
                ResultLimit::FiveThousand => "5,000",
                ResultLimit::TenThousand => "10,000",
            },
            width,
            6,
        ),
    }
}

pub(super) fn search_control_text(form: &SearchForm, width: usize) -> (String, usize) {
    use crate::search::{ContentMode, NameMode, ResultLimit, SearchScope};
    use std::fmt::Write;

    pub(super) fn choice_marker(active: bool, is_selected: bool) -> (&'static str, &'static str) {
        (if active { ">" } else { " " }, selected(is_selected))
    }

    let mut output = String::new();
    match form.section {
        crate::app::SearchSection::Scope => {
            output.push_str("Scope\n");
            let choices = [
                (SearchScope::CurrentDirectory, "Current directory"),
                (SearchScope::RecursiveHere, "Recursive here"),
                (SearchScope::Filesystem, "Entire filesystem"),
            ];
            for (choice, label) in choices {
                let is_selected = form.draft.scope == choice;
                let (active, selected) = choice_marker(is_selected, is_selected);
                let _ = writeln!(output, "{active} {selected} {label}");
            }
            let _ = write!(output, "Root: {}", form.draft.root.display());
            let selected_index = match form.draft.scope {
                SearchScope::CurrentDirectory => 0,
                SearchScope::RecursiveHere => 1,
                SearchScope::Filesystem => 2,
            };
            (output, selected_index + 1)
        }
        crate::app::SearchSection::Match => {
            output.push_str("Match\nName mode\n");
            for (choice, label) in [
                (NameMode::Smart, "Smart"),
                (NameMode::Glob, "Glob"),
                (NameMode::Regex, "Regex"),
            ] {
                let is_selected = form.draft.name_mode == choice;
                let (active, selected) = choice_marker(form.field == 0 && is_selected, is_selected);
                let _ = writeln!(output, "{active} {selected} {label}");
            }
            let _ = writeln!(
                output,
                "{} Content: {}",
                if form.field == 1 { ">" } else { " " },
                cursor_text(
                    &form.draft.content,
                    form.cursors.content,
                    "<optional content>",
                    width.saturating_sub(11),
                )
            );
            output.push_str("Content mode\n");
            for (choice, label) in [
                (ContentMode::Literal, "Literal"),
                (ContentMode::Regex, "Regex"),
            ] {
                let is_selected = form.draft.content_mode == choice;
                let (active, selected) = choice_marker(form.field == 2 && is_selected, is_selected);
                let _ = writeln!(output, "{active} {selected} {label}");
            }
            output.pop();
            let active_line = match form.field {
                0 => {
                    2 + match form.draft.name_mode {
                        NameMode::Smart => 0,
                        NameMode::Glob => 1,
                        NameMode::Regex => 2,
                    }
                }
                1 => 5,
                _ => {
                    7 + match form.draft.content_mode {
                        ContentMode::Literal => 0,
                        ContentMode::Regex => 1,
                    }
                }
            };
            (output, active_line)
        }
        crate::app::SearchSection::Filters => {
            output.push_str("Filters\n");
            for (field, kind, label) in [
                (0, EntryKind::File, "Files"),
                (1, EntryKind::Directory, "Directories"),
                (2, EntryKind::Symlink, "Symlinks"),
                (3, EntryKind::BlockDevice, "Block devices"),
                (4, EntryKind::Other, "Other"),
            ] {
                let _ = writeln!(
                    output,
                    "{} {} {label}",
                    if form.field == field { ">" } else { " " },
                    selected(form.draft.types.contains(kind)),
                );
            }
            for (field, label, value, cursor, reserved) in [
                (
                    5,
                    "Minimum size",
                    form.draft.minimum_size.as_str(),
                    form.cursors.minimum_size,
                    16,
                ),
                (
                    6,
                    "Maximum size",
                    form.draft.maximum_size.as_str(),
                    form.cursors.maximum_size,
                    16,
                ),
                (
                    7,
                    "Modified after",
                    form.draft.modified_after.as_str(),
                    form.cursors.modified_after,
                    18,
                ),
                (
                    8,
                    "Modified before",
                    form.draft.modified_before.as_str(),
                    form.cursors.modified_before,
                    19,
                ),
            ] {
                let _ = writeln!(
                    output,
                    "{} {label}: {}",
                    if form.field == field { ">" } else { " " },
                    cursor_text(value, cursor, "—", width.saturating_sub(reserved)),
                );
            }
            let _ = write!(
                output,
                "{} Include ignored/hidden: {}",
                if form.field == 9 { ">" } else { " " },
                if form.draft.include_ignored_hidden {
                    "Yes"
                } else {
                    "No"
                },
            );
            (output, form.field + 1)
        }
        crate::app::SearchSection::Traversal => {
            output.push_str("Traversal\n");
            for (choice, label) in [
                (ResultLimit::OneThousand, "1,000"),
                (ResultLimit::FiveThousand, "5,000"),
                (ResultLimit::TenThousand, "10,000"),
            ] {
                let is_selected = form.draft.result_limit == choice;
                let (active, selected) = choice_marker(is_selected, is_selected);
                let _ = writeln!(output, "{active} {selected} {label}");
            }
            output.pop();
            let selected_index = match form.draft.result_limit {
                ResultLimit::OneThousand => 0,
                ResultLimit::FiveThousand => 1,
                ResultLimit::TenThousand => 2,
            };
            (output, selected_index + 1)
        }
    }
}

pub(super) fn search_help_text(form: &SearchForm) -> &'static str {
    use crate::search::{ContentMode, NameMode, ResultLimit, SearchScope};

    match (form.section, form.field) {
        (crate::app::SearchSection::Scope, _) => match form.draft.scope {
            SearchScope::CurrentDirectory => {
                "Searches the current directory only. Symbolic links are listed but never followed."
            }
            SearchScope::RecursiveHere => {
                "Searches the current directory and all subfolders. Symbolic links are listed but never followed."
            }
            SearchScope::Filesystem => {
                "Searches recursively from the shown root and all subfolders. Virtual system trees /proc, /sys, /dev, and most of /run are skipped. Symbolic links are listed but never followed."
            }
        },
        (crate::app::SearchSection::Match, 0) => match form.draft.name_mode {
            NameMode::Smart => {
                "Smart matching ranks exact matches first, then prefix, substring, and fuzzy matches."
            }
            NameMode::Glob => {
                "Glob matching accepts shell-style patterns such as *.rs and report-?.txt."
            }
            NameMode::Regex => {
                "Regex matching treats the name query as a regular expression. Invalid expressions are reported before searching."
            }
        },
        (crate::app::SearchSection::Match, 1) => {
            "Content search uses ripgrep and applies only to regular files. Leave it empty to search names and filters only."
        }
        (crate::app::SearchSection::Match, _) => match form.draft.content_mode {
            ContentMode::Literal => {
                "Literal content mode searches for the entered text exactly, without regular-expression syntax."
            }
            ContentMode::Regex => {
                "Regex content mode treats the content query as a regular expression interpreted by ripgrep."
            }
        },
        (crate::app::SearchSection::Filters, 0) => {
            "Files includes regular files when this entry-kind checkbox is enabled."
        }
        (crate::app::SearchSection::Filters, 1) => {
            "Directories includes folders when this entry-kind checkbox is enabled."
        }
        (crate::app::SearchSection::Filters, 2) => {
            "Symlinks includes symbolic-link entries when enabled; their targets are never followed."
        }
        (crate::app::SearchSection::Filters, 3) => {
            "Block devices includes disk and partition device entries when enabled."
        }
        (crate::app::SearchSection::Filters, 4) => {
            "Other includes entry kinds that are not files, directories, symlinks, or block devices."
        }
        (crate::app::SearchSection::Filters, 5) => {
            "The minimum size is inclusive. Accepted examples include 500 B, 20 KB, 5 MB, 1.5 GB, and 2 GiB. Directories are excluded from size filtering."
        }
        (crate::app::SearchSection::Filters, 6) => {
            "The maximum size is inclusive. Accepted examples include 500 B, 20 KB, 5 MB, 1.5 GB, and 2 GiB. Directories are excluded from size filtering."
        }
        (crate::app::SearchSection::Filters, 7) => {
            "Modified after is an inclusive lower bound. Enter YYYY-MM-DD or a relative age such as 7d."
        }
        (crate::app::SearchSection::Filters, 8) => {
            "Modified before is an inclusive upper bound. Enter YYYY-MM-DD or a relative age such as 7d."
        }
        (crate::app::SearchSection::Filters, _) => {
            "Include ignored/hidden searches hidden entries and entries excluded by ignore rules; disabling it respects those rules."
        }
        (crate::app::SearchSection::Traversal, _) => match form.draft.result_limit {
            ResultLimit::OneThousand => {
                "Retains at most 1,000 results to keep memory bounded; additional matches are truncated."
            }
            ResultLimit::FiveThousand => {
                "Retains at most 5,000 results to keep memory bounded; additional matches are truncated."
            }
            ResultLimit::TenThousand => {
                "Retains at most 10,000 results to keep memory bounded; additional matches are truncated."
            }
        },
    }
}

pub(super) fn selected(value: bool) -> &'static str {
    if value {
        "[x]"
    } else {
        "[ ]"
    }
}
pub(super) fn cursor_text(value: &str, cursor: usize, hint: &str, width: usize) -> String {
    cursor_window(value, cursor, hint, width)
}

pub(super) fn cursor_window(value: &str, cursor: usize, hint: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if value.is_empty() {
        let mut output = String::from("│");
        for grapheme in hint.graphemes(true) {
            if UnicodeWidthStr::width(output.as_str()) + UnicodeWidthStr::width(grapheme) > width {
                break;
            }
            output.push_str(grapheme);
        }
        return output;
    }
    let cursor_byte = value
        .char_indices()
        .nth(cursor.min(value.chars().count()))
        .map_or(value.len(), |(index, _)| index);
    let graphemes = value.grapheme_indices(true).collect::<Vec<_>>();
    let caret = graphemes
        .iter()
        .find_map(|(index, grapheme)| {
            let end = index + grapheme.len();
            (cursor_byte > *index && cursor_byte < end).then_some(end)
        })
        .unwrap_or(cursor_byte);
    let content_width = width.saturating_sub(1);
    let left_budget = content_width / 2;
    let mut start = caret;
    let mut used = 0;
    for (index, grapheme) in graphemes.iter().rev().filter(|(index, _)| *index < caret) {
        let grapheme_width = UnicodeWidthStr::width(*grapheme);
        if used + grapheme_width > left_budget {
            break;
        }
        start = *index;
        used += grapheme_width;
    }
    let mut end = caret;
    for (index, grapheme) in graphemes.iter().filter(|(index, _)| *index >= caret) {
        let grapheme_width = UnicodeWidthStr::width(*grapheme);
        if used + grapheme_width > content_width {
            break;
        }
        used += grapheme_width;
        end = index + grapheme.len();
    }
    let initial_start = start;
    for (index, grapheme) in graphemes
        .iter()
        .rev()
        .filter(|(index, _)| *index < initial_start)
    {
        let grapheme_width = UnicodeWidthStr::width(*grapheme);
        if used + grapheme_width > content_width {
            break;
        }
        start = *index;
        used += grapheme_width;
    }
    if end == caret {
        end = caret;
    }
    format!("{}│{}", &value[start..caret], &value[caret..end])
}

pub(super) fn draw_search_results(frame: &mut Frame, app: &App, view: &SearchView) {
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
            "{} result limit reached · ↑/↓/{}/{}: move · Enter: open · Esc: return",
            format_count(view.request.result_limit().get()),
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display()
        )
    } else if view.skipped == 0 {
        format!(
            "↑/↓/{}/{}: move · Enter: open · {}: search here · {}: search filesystem · Esc: return",
            app.config.hotkeys.down.display(),
            app.config.hotkeys.up.display(),
            app.config.hotkeys.search.display(),
            app.config.hotkeys.search_filesystem.display()
        )
    } else {
        format!(
            "↑/↓/{}/{}: move · Enter: open · Esc: return · {} permission error(s) skipped",
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

pub(super) fn format_count(value: usize) -> String {
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
