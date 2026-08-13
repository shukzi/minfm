
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

fn rendered_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    (area.y..area.y.saturating_add(area.height))
        .map(|y| {
            (area.x..area.x.saturating_add(area.width))
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
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
fn quick_search_matches_original_compact_prompt() {
    const SENTINEL: Color = Color::Rgb(0x12, 0x34, 0x56);
    let temp = tempfile::tempdir().unwrap();
    let mut app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Valid {
            config: Config::default(),
            path: temp.path().join("config.toml"),
        },
        false,
    );
    let mut form = SearchForm {
        draft: crate::search::SearchDraft::quick(temp.path().to_path_buf()),
        advanced: false,
        section: crate::app::SearchSection::Match,
        field: 0,
        cursors: crate::app::SearchCursors::default(),
        error: None,
        return_to: crate::app::SearchReturn::Browser,
    };
    form.draft.name = "report".into();
    form.cursors.name = form.draft.name.chars().count();

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(
                Block::default().style(Style::default().bg(SENTINEL)),
                frame.area(),
            );
            draw_quick_search(frame, &app, &form);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(24, 15)].symbol(), "┌");
    assert_eq!(buffer[(95, 15)].symbol(), "┐");
    assert_eq!(buffer[(24, 23)].symbol(), "└");
    assert_eq!(buffer[(95, 23)].symbol(), "┘");
    assert_eq!(buffer[(23, 15)].bg, SENTINEL);
    assert_eq!(buffer[(96, 23)].bg, SENTINEL);
    let rows = rendered_rows(&terminal);
    assert!(rows[15].contains("Search current directory"));
    assert!(rows[16].contains("Enter a value:"));
    assert!(rows.iter().any(|row| row.contains("│> report│")));
    assert!(rows
        .iter()
        .any(|row| row.contains("Enter search · F advanced · Esc cancel")));

    app.config = toml::from_str::<Config>("[hotkeys]\nsearch_filesystem = 'G'").unwrap();
    terminal
        .draw(|frame| draw_quick_search(frame, &app, &form))
        .unwrap();
    let remapped = rendered_text(&terminal);
    assert!(remapped.contains("Enter search · G advanced · Esc cancel"));
    assert!(!remapped.contains("F advanced"));

    form.error = Some("enter a search or choose a filter".into());
    terminal
        .draw(|frame| draw_quick_search(frame, &app, &form))
        .unwrap();
    let validation = rendered_text(&terminal);
    assert!(validation.contains("enter a search or choose a filter"));
    for advanced_only in ["Scope", "Match", "Filters", "Traversal", "Help"] {
        assert!(!validation.contains(advanced_only));
    }
}

#[test]
fn search_dialogs_have_no_popup_halo() {
    const SENTINEL: Color = Color::Rgb(0x12, 0x34, 0x56);
    let temp = tempfile::tempdir().unwrap();
    let app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Valid {
            config: Config::default(),
            path: temp.path().join("config.toml"),
        },
        false,
    );
    let quick = SearchForm {
        draft: crate::search::SearchDraft::quick(temp.path().to_path_buf()),
        advanced: false,
        section: crate::app::SearchSection::Match,
        field: 0,
        cursors: crate::app::SearchCursors::default(),
        error: None,
        return_to: crate::app::SearchReturn::Browser,
    };
    let advanced = SearchForm::advanced(
        temp.path().to_path_buf(),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );

    let mut quick_terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    quick_terminal
        .draw(|frame| {
            frame.render_widget(
                Block::default().style(Style::default().bg(SENTINEL)),
                frame.area(),
            );
            draw_quick_search(frame, &app, &quick);
        })
        .unwrap();
    assert_eq!(quick_terminal.backend().buffer()[(23, 15)].bg, SENTINEL);
    assert!(rendered_text(&quick_terminal).contains("Search current directory"));
    assert_eq!(quick_terminal.backend().buffer()[(24, 15)].symbol(), "┌");

    let mut advanced_terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    advanced_terminal
        .draw(|frame| {
            frame.render_widget(
                Block::default().style(Style::default().bg(SENTINEL)),
                frame.area(),
            );
            draw_advanced_search(frame, &app, &advanced);
        })
        .unwrap();
    assert_eq!(advanced_terminal.backend().buffer()[(4, 4)].bg, SENTINEL);
    assert!(rendered_text(&advanced_terminal).contains("Advanced search"));
    assert_eq!(advanced_terminal.backend().buffer()[(5, 4)].symbol(), "┌");

    let mut popup_terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    popup_terminal
        .draw(|frame| {
            frame.render_widget(
                Block::default().style(Style::default().bg(SENTINEL)),
                frame.area(),
            );
            draw_format_options(frame, "/dev/test", 0, false);
        })
        .unwrap();
    assert_eq!(
        popup_terminal.backend().buffer()[(11, 11)].bg,
        Color::Rgb(0x00, 0x00, 0x00)
    );
}

#[test]
fn search_text_window_keeps_ascii_and_wide_unicode_carets_visible() {
    assert_eq!(cursor_window("abcdefghijk", 10, "", 6), "ghij│k");
    assert_eq!(
        UnicodeWidthStr::width(cursor_window("ab界cd界ef", 6, "", 7).as_str()),
        7
    );
    assert!(cursor_window("ab界cd界ef", 6, "", 7).contains('│'));
    assert_eq!(cursor_window("a\u{301}bcdef", 1, "", 4), "a\u{301}│bc");
}

#[test]
fn active_search_text_controls_window_query_content_size_and_dates() {
    let root = PathBuf::from("/tmp");
    let mut form = SearchForm::advanced(
        root,
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    form.draft.content = "prefix界content-suffix".into();
    form.draft.minimum_size = "12345678901234567890".into();
    form.draft.modified_after = "2026-08-12-and-more".into();
    form.cursors.content = 14;
    form.cursors.minimum_size = 18;
    form.cursors.modified_after = 15;
    for (field, expected) in [(1, "Content"), (5, "Minimum size"), (7, "Modified after")] {
        form.section = if field == 1 {
            crate::app::SearchSection::Match
        } else {
            crate::app::SearchSection::Filters
        };
        form.field = field;
        let rendered = active_search_control_text(&form, 24);
        assert!(rendered.contains(expected));
        assert!(rendered.contains('│'));
        assert!(UnicodeWidthStr::width(rendered.as_str()) <= 24);
    }
}

#[test]
fn search_selector_scope_uses_independent_radio_rows_and_marks_selected_choice() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::RecursiveHere,
        crate::app::SearchReturn::Browser,
    );
    form.section = crate::app::SearchSection::Scope;
    form.field = 0;

    let (rendered, active_line) = search_control_text(&form, 80);

    assert!(
        rendered.contains("  [ ] Current directory\n> [x] Recursive here\n  [ ] Entire filesystem")
    );
    assert_eq!(active_line, 2);
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with('>'))
            .count(),
        1
    );
    assert_eq!(
        rendered.lines().filter(|line| line.contains("[x]")).count(),
        1
    );
}

#[test]
fn search_selector_name_and_content_modes_use_independent_radio_rows() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    form.section = crate::app::SearchSection::Match;

    for (mode, expected, active_line) in [
        (
            crate::search::NameMode::Smart,
            "> [x] Smart\n  [ ] Glob\n  [ ] Regex",
            2,
        ),
        (
            crate::search::NameMode::Glob,
            "  [ ] Smart\n> [x] Glob\n  [ ] Regex",
            3,
        ),
        (
            crate::search::NameMode::Regex,
            "  [ ] Smart\n  [ ] Glob\n> [x] Regex",
            4,
        ),
    ] {
        form.field = 0;
        form.draft.name_mode = mode;
        let (rendered, actual_line) = search_control_text(&form, 80);
        assert!(rendered.contains(expected));
        assert_eq!(actual_line, active_line);
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with('>'))
                .count(),
            1
        );
        assert_eq!(
            rendered.lines().filter(|line| line.contains("[x]")).count(),
            2
        );
    }

    for (mode, expected, active_line) in [
        (
            crate::search::ContentMode::Literal,
            "> [x] Literal\n  [ ] Regex",
            7,
        ),
        (
            crate::search::ContentMode::Regex,
            "  [ ] Literal\n> [x] Regex",
            8,
        ),
    ] {
        form.field = 2;
        form.draft.content_mode = mode;
        let (rendered, actual_line) = search_control_text(&form, 80);
        assert!(rendered.contains(expected));
        assert_eq!(actual_line, active_line);
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with('>'))
                .count(),
            1
        );
        assert_eq!(
            rendered.lines().filter(|line| line.contains("[x]")).count(),
            2
        );
    }
}

#[test]
fn search_selector_result_limits_use_independent_radio_rows() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    form.section = crate::app::SearchSection::Traversal;
    form.field = 0;

    for (limit, expected, active_line) in [
        (
            crate::search::ResultLimit::OneThousand,
            "> [x] 1,000\n  [ ] 5,000\n  [ ] 10,000",
            1,
        ),
        (
            crate::search::ResultLimit::FiveThousand,
            "  [ ] 1,000\n> [x] 5,000\n  [ ] 10,000",
            2,
        ),
        (
            crate::search::ResultLimit::TenThousand,
            "  [ ] 1,000\n  [ ] 5,000\n> [x] 10,000",
            3,
        ),
    ] {
        form.draft.result_limit = limit;
        let (rendered, actual_line) = search_control_text(&form, 80);
        assert!(rendered.contains(expected));
        assert_eq!(actual_line, active_line);
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with('>'))
                .count(),
            1
        );
        assert_eq!(
            rendered.lines().filter(|line| line.contains("[x]")).count(),
            1
        );
    }
}

#[test]
fn search_selector_entry_kinds_are_five_independent_checkbox_rows() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    form.section = crate::app::SearchSection::Filters;
    form.field = 2;
    form.draft.types.toggle(EntryKind::File);
    form.draft.types.toggle(EntryKind::Symlink);
    form.draft.types.toggle(EntryKind::Other);

    let (rendered, active_line) = search_control_text(&form, 80);

    assert!(rendered.contains(
        "  [x] Files\n  [ ] Directories\n> [x] Symlinks\n  [ ] Block devices\n  [x] Other"
    ));
    assert_eq!(active_line, 3);
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with('>'))
            .count(),
        1
    );
}

#[test]
fn search_help_covers_every_search_control() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    for (section, field_count) in [
        (crate::app::SearchSection::Scope, 1),
        (crate::app::SearchSection::Match, 3),
        (crate::app::SearchSection::Filters, 10),
        (crate::app::SearchSection::Traversal, 1),
    ] {
        form.section = section;
        for field in 0..field_count {
            form.field = field;
            assert!(
                !search_help_text(&form).trim().is_empty(),
                "missing help for {section:?} field {field}"
            );
        }
    }
}

#[test]
fn search_help_documents_search_semantics_and_accepted_forms() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::RecursiveHere,
        crate::app::SearchReturn::Browser,
    );
    let scope_help = search_help_text(&form);
    assert!(scope_help.contains("all subfolders"));
    assert!(scope_help.contains("never followed"));

    form.section = crate::app::SearchSection::Filters;
    form.field = 5;
    let minimum_size_help = search_help_text(&form);
    assert!(minimum_size_help.contains("500 B"));
    assert!(minimum_size_help.contains("1.5 GB"));
    assert!(minimum_size_help.contains("2 GiB"));
    assert!(minimum_size_help.contains("inclusive"));
    assert!(minimum_size_help.contains("Directories"));

    form.field = 7;
    let modified_after_help = search_help_text(&form);
    assert!(modified_after_help.contains("YYYY-MM-DD"));
    assert!(modified_after_help.contains("7d"));
    assert!(modified_after_help.contains("inclusive"));

    form.section = crate::app::SearchSection::Match;
    form.field = 1;
    assert!(search_help_text(&form).contains("ripgrep"));
}

#[test]
fn search_help_changes_with_selected_scope_and_name_mode() {
    let mut form = SearchForm::advanced(
        PathBuf::from("/tmp"),
        crate::search::SearchScope::CurrentDirectory,
        crate::app::SearchReturn::Browser,
    );
    let current = search_help_text(&form);
    form.draft.scope = crate::search::SearchScope::RecursiveHere;
    let recursive = search_help_text(&form);
    form.draft.scope = crate::search::SearchScope::Filesystem;
    let filesystem = search_help_text(&form);
    assert_ne!(current, recursive);
    assert_ne!(recursive, filesystem);
    assert!(current.contains("current directory only"));
    assert!(recursive.contains("all subfolders"));
    assert!(filesystem.contains("shown root and all subfolders"));
    for skipped_tree in ["/proc", "/sys", "/dev", "/run"] {
        assert!(filesystem.contains(skipped_tree));
    }
    assert!(filesystem.contains("never followed"));

    form.section = crate::app::SearchSection::Match;
    form.field = 0;
    form.draft.name_mode = crate::search::NameMode::Smart;
    let smart = search_help_text(&form);
    form.draft.name_mode = crate::search::NameMode::Glob;
    let glob = search_help_text(&form);
    form.draft.name_mode = crate::search::NameMode::Regex;
    let regex = search_help_text(&form);
    assert_ne!(smart, glob);
    assert_ne!(glob, regex);
    for tier in ["exact", "prefix", "substring", "fuzzy"] {
        assert!(smart.contains(tier));
    }
    assert!(glob.contains("*.rs"));
    assert!(regex.contains("regular expression"));
}

#[test]
fn search_advanced_wide_renders_vertical_controls_and_contextual_help_for_each_section() {
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
    for (section, expected_rows, help) in [
        (
            crate::app::SearchSection::Scope,
            &[
                "[x] Current directory",
                "[ ] Recursive here",
                "[ ] Entire filesystem",
            ][..],
            "current directory only",
        ),
        (
            crate::app::SearchSection::Match,
            &["[x] Smart", "[ ] Glob", "[ ] Regex"][..],
            "exact matches first",
        ),
        (
            crate::app::SearchSection::Filters,
            &["[x] Files", "[x] Directories", "[x] Symlinks"][..],
            "regular files",
        ),
        (
            crate::app::SearchSection::Traversal,
            &["[ ] 1,000", "[x] 5,000", "[ ] 10,000"][..],
            "Retains at most",
        ),
    ] {
        form.section = section;
        form.field = 0;
        app.mode = AppMode::SearchForm(form.clone());
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rows = rendered_rows(&terminal);
        for expected in expected_rows {
            assert!(
                rows.iter().any(|row| row.contains(expected)),
                "missing separate row {expected} for {section:?}: {rows:?}"
            );
        }
        assert!(
            rows.iter().any(|row| row.contains("Help")),
            "missing Help title"
        );
        assert!(
            rendered_text(&terminal).contains(help),
            "missing active help for {section:?}"
        );
    }
}

#[test]
fn search_advanced_narrow_renders_size_help_inside_the_outer_border() {
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
        "KB",
        "GB",
        "GiB",
        "inclusive",
        "invalid minimum size",
        "Enter search",
        "Esc cancel",
    ] {
        assert!(text.contains(label), "missing {label}");
    }
    let rows = rendered_rows(&terminal);
    for row in rows.iter().filter(|row| {
        ["KB", "GB", "GiB", "inclusive"]
            .iter()
            .any(|needle| row.contains(needle))
    }) {
        let first = row.find('│').expect("advanced-search left border");
        let last = row.rfind('│').expect("advanced-search right border");
        assert!(
            first < last,
            "help must remain inside outer border: {row:?}"
        );
    }
}

#[test]
fn search_advanced_short_preserves_active_control_error_footer_and_caret() {
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
    form.draft.minimum_size = "500".into();
    form.cursors.minimum_size = 3;
    form.error = Some("invalid size".into());
    app.mode = AppMode::SearchForm(form);
    let mut terminal = Terminal::new(TestBackend::new(52, 14)).unwrap();

    terminal.draw(|frame| draw(frame, &app)).unwrap();

    let text = rendered_text(&terminal);
    for expected in [
        "Minimum size",
        "500│",
        "invalid size",
        "Enter search",
        "Esc cancel",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
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
        if width == 40 {
            assert!(
                text.contains("> Include ignore"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("d/hidden: No"), "{width}x{height}: {text}");
        } else {
            assert!(
                text.contains("ignored/hidden: No"),
                "{width}x{height}: {text}"
            );
        }
    }
}

#[test]
fn search_advanced_one_row_keeps_every_active_control_recognizable() {
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
    form.draft.scope = crate::search::SearchScope::Filesystem;
    form.draft.name_mode = crate::search::NameMode::Regex;
    form.draft.content_mode = crate::search::ContentMode::Literal;
    form.draft.result_limit = crate::search::ResultLimit::TenThousand;
    form.draft.include_ignored_hidden = true;
    form.draft.content = "long界content-value".into();
    form.draft.minimum_size = "18446744073709551615B".into();
    form.draft.maximum_size = "18446744073709551615B".into();
    form.draft.modified_after = "2026-08-12".into();
    form.draft.modified_before = "2026-08-12".into();
    form.cursors.content = form.draft.content.chars().count();
    form.cursors.minimum_size = form.draft.minimum_size.chars().count();
    form.cursors.maximum_size = form.draft.maximum_size.chars().count();
    form.cursors.modified_after = form.draft.modified_after.chars().count();
    form.cursors.modified_before = form.draft.modified_before.chars().count();

    for (section, field, label, value, text_control) in [
        (
            crate::app::SearchSection::Scope,
            0,
            "Scope",
            "Entire",
            false,
        ),
        (crate::app::SearchSection::Match, 0, "Name", "Regex", false),
        (crate::app::SearchSection::Match, 1, "Content", "", true),
        (
            crate::app::SearchSection::Match,
            2,
            "Content md",
            "Li",
            false,
        ),
        (crate::app::SearchSection::Filters, 0, "Files", "[x]", false),
        (
            crate::app::SearchSection::Filters,
            1,
            "Director",
            "[x]",
            false,
        ),
        (
            crate::app::SearchSection::Filters,
            2,
            "Symlinks",
            "[x]",
            false,
        ),
        (crate::app::SearchSection::Filters, 3, "Block", "[x]", false),
        (crate::app::SearchSection::Filters, 4, "Other", "[x]", false),
        (crate::app::SearchSection::Filters, 5, "Minimum", "", true),
        (crate::app::SearchSection::Filters, 6, "Maximum", "", true),
        (crate::app::SearchSection::Filters, 7, "After", "", true),
        (crate::app::SearchSection::Filters, 8, "Before", "", true),
        (
            crate::app::SearchSection::Filters,
            9,
            "Include",
            "Yes",
            false,
        ),
        (
            crate::app::SearchSection::Traversal,
            0,
            "Limit",
            "10,000",
            false,
        ),
    ] {
        form.section = section;
        form.field = field;
        app.mode = AppMode::SearchForm(form.clone());
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rows = rendered_rows(&terminal);
        let active = rows
            .iter()
            .find(|row| row.contains('>'))
            .unwrap_or_else(|| panic!("missing active marker for {section:?}/{field}: {rows:?}"));
        assert!(
            active.contains(label),
            "missing label {label} for {section:?}/{field}: {active:?}"
        );
        assert!(
            value.is_empty() || active.contains(value),
            "missing value {value} for {section:?}/{field}: {active:?}"
        );
        assert!(
            !text_control || active.contains('│'),
            "missing caret for {section:?}/{field}: {active:?}"
        );
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
        for form in [
            SearchForm {
                draft: crate::search::SearchDraft::quick(temp.path().to_path_buf()),
                advanced: false,
                section: crate::app::SearchSection::Match,
                field: 0,
                cursors: crate::app::SearchCursors::default(),
                error: None,
                return_to: crate::app::SearchReturn::Browser,
            },
            SearchForm::advanced(
                temp.path().to_path_buf(),
                crate::search::SearchScope::CurrentDirectory,
                crate::app::SearchReturn::Browser,
            ),
        ] {
            app.mode = AppMode::SearchForm(form);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
        }
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
        entries: partition::from_lsblk_fixture(fixture, &[std::path::PathBuf::from("/dev/sda1")])
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

/// Manual baseline (2026-08-12, release build): 89 us at 10,000 retained
/// results, 80x24 terminal, median of nine visible-row renders.
#[test]
#[ignore]
fn benchmark_search_render_visible() {
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
    draft.name = "result".into();
    let retained_results = crate::search::ResultLimit::TenThousand.get();
    let results = (0..retained_results)
        .map(|index| {
            let path = temp.path().join(format!("result-{index:05}.txt"));
            std::fs::write(&path, []).unwrap();
            crate::search::hit_for_test(path, "result")
        })
        .collect();
    app.search_results = Some(crate::app::SearchView {
        request: draft.compile(true).unwrap(),
        results,
        selected: retained_results / 2,
        selected_path: None,
        skipped: 0,
        truncated: true,
        incomplete: false,
    });
    app.mode = AppMode::SearchResults;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut samples = Vec::with_capacity(9);
    for _ in 0..9 {
        let started = Instant::now();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        samples.push(started.elapsed().as_micros());
    }
    samples.sort_unstable();
    let median = samples[4];
    eprintln!(
            "PERF search_render_visible_us={median} retained_results={retained_results} visible_rows=18"
        );
    assert_eq!(
        app.search_results.as_ref().unwrap().results.len(),
        retained_results
    );
}
