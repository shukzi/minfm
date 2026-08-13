
use super::*;
use std::{
    ffi::OsString,
    fs::File,
    os::unix::ffi::OsStringExt,
    os::unix::fs::PermissionsExt,
    sync::{atomic::AtomicBool, Arc},
};

fn wait_for_search(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.search.is_some() && Instant::now() < deadline {
        if !app.poll_search() {
            thread::yield_now();
        }
    }
    assert!(app.search.is_none(), "search worker timed out");
}

fn test_app(root: &Path) -> App {
    let config = Config::default();
    App::new(
        root.to_path_buf(),
        ConfigLoad::Valid {
            config,
            path: root.join("config.toml"),
        },
        false,
    )
}

fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

#[test]
fn browser_and_search_result_selection_share_mark_then_cursor_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ["one.txt", "two.txt", "three.txt"].map(|name| temp.path().join(name));
    for path in &paths {
        std::fs::write(path, b"").unwrap();
    }
    let mut entries = paths
        .iter()
        .map(|path| search::hit_for_test(path.clone(), "txt").entry)
        .collect::<Vec<_>>();
    entries[0].selected = true;
    entries[1].selected = true;
    let mut hits = entries
        .iter()
        .cloned()
        .map(|entry| search::hit_for_test(entry.path, "txt"))
        .collect::<Vec<_>>();
    hits[0].entry.selected = true;
    hits[1].entry.selected = true;

    assert_eq!(target_paths_from(&entries, 2), paths[..2]);
    assert_eq!(target_paths_from_hits(&hits, 2), paths[..2]);
    entries.iter_mut().for_each(|entry| entry.selected = false);
    hits.iter_mut().for_each(|hit| hit.entry.selected = false);
    assert_eq!(target_paths_from(&entries, 2), vec![paths[2].clone()]);
    assert_eq!(target_paths_from_hits(&hits, 2), vec![paths[2].clone()]);
    assert!(std::ptr::eq(
        focused_entry_from(&entries, 2).unwrap(),
        &entries[2]
    ));
}

fn install_search_results(app: &mut App, paths: &[PathBuf]) {
    let mut draft = SearchDraft::quick(app.current_dir.clone());
    draft.name = "txt".into();
    app.search_results = Some(SearchView {
        request: draft.compile(true).unwrap(),
        results: paths
            .iter()
            .map(|path| search::hit_for_test(path.clone(), "txt"))
            .collect(),
        selected: 0,
        selected_path: paths.first().cloned(),
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchResults;
}

#[test]
fn search_results_select_copy_and_unavailable_creation_use_result_state() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"").unwrap();
    std::fs::write(&second, b"").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &[first.clone(), second.clone()]);

    app.handle_key(key(' '));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(key(' '));
    app.handle_key(key('c'));
    assert_eq!(app.clipboard.as_ref().unwrap().paths, vec![first, second]);
    assert!(matches!(app.mode, AppMode::SearchResults));

    app.handle_key(key('n'));
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert_eq!(app.status, "Unavailable in search results");
}

#[test]
fn search_result_copy_revalidates_mixed_marked_targets() {
    let temp = tempfile::tempdir().unwrap();
    let valid = temp.path().join("valid.txt");
    let stale = temp.path().join("stale.txt");
    std::fs::write(&valid, b"updated contents").unwrap();
    std::fs::write(&stale, b"stale").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &[valid.clone(), stale.clone()]);
    let view = app.search_results.as_mut().unwrap();
    view.results[0].entry.selected = true;
    view.results[1].entry.selected = true;
    view.selected = 1;
    view.selected_path = Some(stale.clone());
    std::fs::remove_file(&stale).unwrap();

    app.handle_key(key('c'));

    assert_eq!(app.clipboard.as_ref().unwrap().paths, vec![valid.clone()]);
    let view = app.search_results.as_ref().unwrap();
    assert_eq!(view.results.len(), 1);
    assert_eq!(view.results[0].entry.path, valid);
    assert!(view.results[0].entry.selected);
    assert_eq!(view.results[0].entry.size, b"updated contents".len() as u64);
    assert_eq!(view.selected, 0);
    assert!(app.visible_status().contains("no longer available"));
}

#[test]
fn search_result_cut_revalidates_mixed_marked_targets() {
    let temp = tempfile::tempdir().unwrap();
    let valid = temp.path().join("valid.txt");
    let stale = temp.path().join("stale.txt");
    std::fs::write(&valid, b"valid").unwrap();
    std::fs::write(&stale, b"stale").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &[valid.clone(), stale.clone()]);
    for hit in &mut app.search_results.as_mut().unwrap().results {
        hit.entry.selected = true;
    }
    std::fs::remove_file(stale).unwrap();

    app.handle_key(key('x'));

    let clipboard = app.clipboard.as_ref().unwrap();
    assert!(matches!(clipboard.mode, ClipboardMode::Cut));
    assert_eq!(clipboard.paths, vec![valid]);
    assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);
}

#[test]
fn search_result_info_revalidates_current_metadata_and_removes_stale_entry() {
    let temp = tempfile::tempdir().unwrap();
    let current = temp.path().join("current.txt");
    std::fs::write(&current, b"old").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&current));
    std::fs::write(&current, b"new metadata").unwrap();

    app.handle_key(key('I'));

    assert!(matches!(
        &app.mode,
        AppMode::Info(Some(entry)) if entry.path == current && entry.size == b"new metadata".len() as u64
    ));
    app.mode = AppMode::SearchResults;
    std::fs::remove_file(&current).unwrap();
    app.handle_key(key('I'));
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert!(app.visible_status().contains("no longer available"));
}

#[test]
fn search_result_rename_returns_to_results_and_refreshes_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.txt");
    let new = temp.path().join("new.txt");
    std::fs::write(&old, b"old").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&old));

    app.handle_key(key('r'));
    assert!(matches!(app.mode, AppMode::Prompt(Prompt::Rename { .. })));
    if let AppMode::Prompt(Prompt::Rename { input, cursor, .. }) = &mut app.mode {
        *input = "new.txt".into();
        *cursor = input.len();
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert_eq!(
        app.search_results.as_ref().unwrap().results[0].entry.path,
        new
    );
    assert!(!old.exists());
}

#[test]
fn stale_search_result_is_removed_before_rename() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("gone.txt");
    std::fs::write(&path, b"").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&path));
    std::fs::remove_file(path).unwrap();

    app.handle_key(key('r'));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert_eq!(app.status, "Search result is no longer available");
}

#[test]
fn stale_result_activation_revalidates_before_using_snapshot_kind() {
    let temp = tempfile::tempdir().unwrap();
    let deleted_file = temp.path().join("deleted.txt");
    let replaced_directory = temp.path().join("was-directory.txt");
    std::fs::write(&deleted_file, b"").unwrap();
    std::fs::create_dir(&replaced_directory).unwrap();
    let mut app = test_app(temp.path());
    install_search_results(
        &mut app,
        &[deleted_file.clone(), replaced_directory.clone()],
    );
    std::fs::remove_file(&deleted_file).unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);

    std::fs::remove_dir(&replaced_directory).unwrap();
    std::fs::write(&replaced_directory, b"now a file").unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert!(app.status.contains("changed type"));
}

#[test]
fn stale_archive_result_is_not_reinterpreted_as_a_creation_source() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("old.txt.zip");
    std::fs::write(&archive, b"not needed").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&archive));
    std::fs::remove_file(&archive).unwrap();

    app.handle_key(key('z'));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert_eq!(app.status, "Search result is no longer available");
}

#[test]
fn archive_result_revalidation_skips_changed_type_but_keeps_valid_marked_sources() {
    let temp = tempfile::tempdir().unwrap();
    let valid = temp.path().join("valid.txt");
    let changed = temp.path().join("changed.txt");
    std::fs::write(&valid, b"valid").unwrap();
    std::fs::write(&changed, b"old").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &[valid.clone(), changed.clone()]);
    for hit in &mut app.search_results.as_mut().unwrap().results {
        hit.entry.selected = true;
    }
    std::fs::remove_file(&changed).unwrap();
    std::fs::create_dir(&changed).unwrap();

    app.handle_key(key('z'));

    assert!(
        matches!(app.mode, AppMode::Prompt(Prompt::ArchiveFormat { ref sources, .. }) if sources == &vec![valid])
    );
    assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);
    assert!(app.status.contains("changed type"));
}

#[test]
fn archive_result_revalidation_does_not_follow_or_reclassify_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.txt");
    let link = temp.path().join("linked.txt");
    std::fs::write(&target, b"target").unwrap();
    symlink(&target, &link).unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&link));

    app.handle_key(key('z'));

    assert!(
        matches!(app.mode, AppMode::Prompt(Prompt::ArchiveFormat { ref sources, .. }) if sources == &vec![link])
    );
}

#[test]
fn missing_search_result_is_removed_before_trash_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.txt");
    std::fs::write(&missing, b"stale").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&missing));
    std::fs::remove_file(&missing).unwrap();

    app.handle_key(key('d'));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert_eq!(app.status, "Search result is no longer available");
}

#[test]
fn file_replaced_by_directory_is_removed_before_trash_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let replaced = temp.path().join("replaced.txt");
    std::fs::write(&replaced, b"file snapshot").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&replaced));
    std::fs::remove_file(&replaced).unwrap();
    std::fs::create_dir(&replaced).unwrap();

    app.handle_key(key('d'));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(replaced.is_dir());
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert_eq!(
        app.status,
        "Search result changed type and is no longer available"
    );
}

#[test]
fn directory_replaced_by_file_is_removed_before_quick_trash() {
    let temp = tempfile::tempdir().unwrap();
    let replaced = temp.path().join("replaced.txt");
    std::fs::create_dir(&replaced).unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&replaced));
    std::fs::remove_dir(&replaced).unwrap();
    std::fs::write(&replaced, b"replacement file").unwrap();

    app.handle_key(key('D'));

    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(replaced.is_file());
    assert!(app.operation.is_none());
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert_eq!(
        app.status,
        "Search result changed type and is no longer available"
    );
}

#[test]
fn trash_confirmation_keeps_only_valid_marked_search_results() {
    let temp = tempfile::tempdir().unwrap();
    let valid = temp.path().join("valid.txt");
    let missing = temp.path().join("missing.txt");
    let changed = temp.path().join("changed.txt");
    std::fs::write(&valid, b"valid").unwrap();
    std::fs::write(&missing, b"missing").unwrap();
    std::fs::write(&changed, b"file snapshot").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &[valid.clone(), missing.clone(), changed.clone()]);
    for hit in &mut app.search_results.as_mut().unwrap().results {
        hit.entry.selected = true;
    }
    std::fs::remove_file(&missing).unwrap();
    std::fs::remove_file(&changed).unwrap();
    std::fs::create_dir(&changed).unwrap();

    app.handle_key(key('d'));

    assert!(
        matches!(&app.mode, AppMode::Prompt(Prompt::ConfirmTrash { paths }) if paths == &vec![valid.clone()])
    );
    let view = app.search_results.as_ref().unwrap();
    assert_eq!(view.results.len(), 1);
    assert_eq!(view.results[0].entry.path, valid);
    assert!(view.results[0].entry.selected);
    assert!(changed.is_dir());
    assert!(app.status.contains("changed type"));
}

#[test]
fn refresh_keeps_mark_and_moves_cursor_to_nearest_remaining_result() {
    let temp = tempfile::tempdir().unwrap();
    let paths = ["one.txt", "two.txt", "three.txt"].map(|name| temp.path().join(name));
    for path in &paths {
        std::fs::write(path, b"").unwrap();
    }
    let mut app = test_app(temp.path());
    install_search_results(&mut app, &paths);
    {
        let view = app.search_results.as_mut().unwrap();
        view.selected = 1;
        view.results[2].entry.selected = true;
    }
    std::fs::remove_file(&paths[1]).unwrap();

    app.refresh_search_results(None);

    let view = app.search_results.as_ref().unwrap();
    assert_eq!(view.selected, 1);
    assert_eq!(view.results[1].entry.path, paths[2]);
    assert!(view.results[1].entry.selected);
}

#[test]
fn cut_result_pasted_from_browser_refreshes_retained_results_after_move() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination");
    std::fs::write(&source, b"move me").unwrap();
    std::fs::create_dir(&destination).unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&source));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.go_to(destination.clone());

    app.handle_key(key('p'));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.operation.is_some() && Instant::now() < deadline {
        if !app.poll_operation() {
            thread::yield_now();
        }
    }

    assert!(destination.join("source.txt").is_file());
    assert!(app.search_results.as_ref().unwrap().results.is_empty());
    assert!(matches!(app.mode, AppMode::Browser));
}

#[test]
fn read_only_result_mutations_stay_in_results() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    std::fs::write(&source, b"").unwrap();
    let mut app = test_app(temp.path());
    app.config.behavior.read_only = true;
    install_search_results(&mut app, std::slice::from_ref(&source));

    for key_code in ['x', 'c', 'd', 'D', 'z'] {
        app.handle_key(key(key_code));
        assert!(matches!(app.mode, AppMode::SearchResults));
    }
    assert!(source.exists());
    assert!(app.clipboard.is_none());
}

#[test]
fn search_result_terminal_editor_and_launch_error_return_to_results() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notes.txt");
    std::fs::write(&path, b"notes").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&path));
    app.config.open.editor = "vim".into();

    app.handle_key(key('e'));
    let action = app.take_terminal_editor().unwrap();
    assert_eq!(action.return_to, ReturnDestination::SearchResults);
    app.finish_terminal_editor(&action, Ok(()));
    assert!(matches!(app.mode, AppMode::SearchResults));

    app.launch_sender
        .try_send(PendingLaunchError {
            error: LaunchError {
                program: "opener".into(),
                path,
                detail: "failed".into(),
            },
            return_to: ReturnDestination::SearchResults,
        })
        .unwrap();
    assert!(app.poll_file_launch());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
}

#[test]
fn overlapping_launcher_errors_keep_their_individual_return_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    for (detail, return_to) in [
        ("browser error", ReturnDestination::Browser),
        ("results error", ReturnDestination::SearchResults),
    ] {
        app.launch_sender
            .try_send(PendingLaunchError {
                error: LaunchError {
                    program: "opener".into(),
                    path: temp.path().join("file.txt"),
                    detail: detail.into(),
                },
                return_to,
            })
            .unwrap();
    }
    app.mode = AppMode::SearchResults;

    assert!(app.poll_file_launch());
    assert!(
        matches!(app.mode, AppMode::Prompt(Prompt::OpenError { ref body, .. }) if body.contains("browser error"))
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
    assert!(app.poll_file_launch());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
}

#[test]
fn search_result_info_closes_back_to_results_with_context() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("notes.txt");
    std::fs::write(&path, b"notes").unwrap();
    let mut app = test_app(temp.path());
    install_search_results(&mut app, std::slice::from_ref(&path));

    app.handle_key(key('I'));
    assert!(matches!(app.mode, AppMode::Info(Some(ref entry)) if entry.path == path));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
}

fn test_network_environment(root: &Path) -> NetworkEnvironment {
    let gio = root.join("gio");
    std::fs::write(
        &gio,
        "#!/bin/sh\nif [ \"$1\" = list ]; then exit 0; fi\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&gio).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&gio, permissions).unwrap();
    NetworkEnvironment {
        gio,
        secret_tool: None,
        runtime_dir: root.join("runtime"),
        shares_file: root.join("network-shares.toml"),
    }
}

fn wait_for_archive(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.archive_operation.is_some() && Instant::now() < deadline {
        if !app.poll_archive() {
            thread::yield_now();
        }
    }
    assert!(app.archive_operation.is_none(), "archive worker timed out");
}

#[test]
fn archive_hotkey_creates_and_inspects_an_archive_without_external_tools() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    std::fs::write(&source, b"archive me").unwrap();
    let mut app = test_app(temp.path());
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == source)
        .unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::ArchiveFormat { selected: 0, .. })
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::ArchiveName {
            format: ArchiveFormat::TarGz,
            ..
        })
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Progress));
    wait_for_archive(&mut app);

    let archive = temp.path().join("source.txt.tar.gz");
    assert!(archive.is_file());
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == archive)
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::ArchiveActions { selected: 0, .. })
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_archive(&mut app);
    assert!(matches!(
        app.mode,
        AppMode::Archive(ArchiveView { ref entries, .. })
            if entries.iter().any(|entry| entry.path == Path::new("source.txt"))
    ));
}

#[test]
fn successful_multi_selection_archive_clears_marked_sources() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let mut app = test_app(temp.path());
    for entry in &mut app.entries {
        entry.selected = entry.path == first || entry.path == second;
    }

    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_archive(&mut app);

    let archive = temp.path().join("archive.tar.gz");
    assert!(archive.is_file());
    assert!(app.entries.iter().all(|entry| !entry.selected));
    assert_eq!(
        app.selected_entry().map(|entry| entry.path.as_path()),
        Some(archive.as_path())
    );
}

#[test]
fn archive_actions_respect_read_only_mode_but_allow_inspection() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("bundle.tar");
    let file = File::create(&archive).unwrap();
    tar::Builder::new(file).finish().unwrap();
    let mut app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Valid {
            config: Config::default(),
            path: temp.path().join("config.toml"),
        },
        true,
    );
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == archive)
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
    assert_eq!(app.status, "Read-only mode: archive extraction is disabled");

    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_archive(&mut app);
    assert!(matches!(app.mode, AppMode::Archive(_)));
}

#[test]
fn archive_progress_escape_requests_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    app.archive_operation = Some(RunningArchive {
        receiver,
        cancel: Arc::clone(&cancel),
    });
    app.progress.cancellable = true;
    app.mode = AppMode::Progress;
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(cancel.load(Ordering::Relaxed));
    assert!(app.progress.cancelling);
    drop(sender);
}

#[test]
fn archive_names_are_single_safe_filenames() {
    assert!(validate_archive_name("backup.tar.gz").is_ok());
    assert!(validate_archive_name("").is_err());
    assert!(validate_archive_name("../backup.zip").is_err());
    assert!(validate_archive_name("folder/backup.zip").is_err());
    assert_eq!(
        suggested_archive_name(&[PathBuf::from("photos")], ArchiveFormat::Zip),
        "photos.zip"
    );
}

#[test]
fn modal_isolation_prevents_browser_shortcuts() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("untouched.txt");
    std::fs::write(&file, b"safe").unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Prompt(Prompt::GoTo {
        input: String::new(),
    });

    for ch in ['d', 'D', 'x', 'c', 'p', 'z', 'r', 'm', 'v', 'q'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    assert!(file.exists());
    assert!(app.operation.is_none());
    assert!(app.running);
    assert!(matches!(app.mode, AppMode::Prompt(Prompt::GoTo { .. })));
}

#[test]
fn update_confirmation_ignores_file_shortcuts() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("untouched.txt");
    std::fs::write(&file, b"safe").unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Prompt(Prompt::UpdateAvailable {
        current: "0.1.2".into(),
        latest: "v0.1.3".into(),
    });

    for ch in ['d', 'D', 'x', 'c', 'p', 'r', 'm', 'q'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    assert!(file.exists());
    assert!(app.update.is_none());
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::UpdateAvailable { .. })
    ));
}

#[test]
fn update_prompt_prefixes_both_versions_with_v() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.pending_update = Some("v9.9.9".into());

    assert!(app.poll_update());
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::UpdateAvailable {
            current,
            latest,
        }) if current == format!("v{}", env!("CARGO_PKG_VERSION")) && latest == "v9.9.9"
    ));
}

#[test]
fn invalid_config_only_accepts_config_actions() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("untouched.txt");
    std::fs::write(&file, b"safe").unwrap();
    let mut app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Invalid {
            path: temp.path().join("bad.toml"),
            error: "bad".into(),
        },
        false,
    );
    for ch in ['d', 'D', 'x', 'c', 'p', 'm'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert!(file.exists());
    assert!(matches!(app.mode, AppMode::ConfigError { .. }));
}

#[test]
fn asynchronous_open_errors_use_a_modal() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.launch_sender
        .try_send(PendingLaunchError {
            error: LaunchError {
                program: "missing-opener".into(),
                path: temp.path().join("example.txt"),
                detail: "application not found".into(),
            },
            return_to: ReturnDestination::Browser,
        })
        .unwrap();

    assert!(app.poll_file_launch());
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::OpenError {
            config_error: None,
            ..
        })
    ));
}

#[test]
fn immediate_open_failure_is_not_overwritten_by_browser_mode() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("example.txt"), b"example").unwrap();
    let mut app = test_app(temp.path());
    app.config.open.opener = "/minfm-test/missing-opener".into();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::OpenError {
            config_error: None,
            ..
        })
    ));
}

#[test]
fn editor_shortcut_is_contextual_to_text_files() {
    let temp = tempfile::tempdir().unwrap();
    let text = temp.path().join("notes.txt");
    let image = temp.path().join("image.png");
    std::fs::write(&text, b"notes").unwrap();
    std::fs::write(&image, b"not a real image").unwrap();
    let mut app = test_app(temp.path());
    app.config.open.editor = "/minfm-test/missing-editor".into();

    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == image)
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));

    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == text)
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::OpenError { .. })
    ));
}

#[test]
fn terminal_editor_restores_selector_and_multi_selection() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    let mut app = test_app(temp.path());
    app.config.open.editor = "nano".into();
    app.entries
        .iter_mut()
        .find(|entry| entry.path == first)
        .unwrap()
        .selected = true;
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == second)
        .unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let action = app.take_terminal_editor().expect("terminal editor queued");
    assert_eq!(action.program(), "nano");
    assert_eq!(action.path(), second);
    assert!(matches!(app.mode, AppMode::Browser));

    std::fs::write(&second, b"edited contents").unwrap();
    std::fs::write(temp.path().join("new.txt"), b"new").unwrap();
    app.finish_terminal_editor(&action, Ok(()));

    assert_eq!(app.selected_entry().map(|entry| &entry.path), Some(&second));
    assert!(
        app.entries
            .iter()
            .find(|entry| entry.path == first)
            .unwrap()
            .selected
    );
    assert_eq!(
        app.entries
            .iter()
            .find(|entry| entry.path == second)
            .unwrap()
            .size,
        b"edited contents".len() as u64
    );
}

#[test]
fn terminal_editor_failure_returns_to_an_error_modal() {
    let temp = tempfile::tempdir().unwrap();
    let text = temp.path().join("notes.txt");
    std::fs::write(&text, b"notes").unwrap();
    let mut app = test_app(temp.path());
    app.config.open.editor = "vim".into();
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == text)
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let action = app.take_terminal_editor().unwrap();

    app.finish_terminal_editor(
        &action,
        Err(LaunchError {
            program: "vim".into(),
            path: text,
            detail: "editor failed".into(),
        }),
    );

    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::OpenError {
            config_error: None,
            ..
        })
    ));
}

#[test]
fn open_error_returns_to_invalid_configuration_screen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bad.toml");
    let mut app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Invalid {
            path: path.clone(),
            error: "invalid value".into(),
        },
        false,
    );
    app.launch_sender
        .try_send(PendingLaunchError {
            error: LaunchError {
                program: "missing-editor".into(),
                path: path.clone(),
                detail: "application not found".into(),
            },
            return_to: ReturnDestination::Browser,
        })
        .unwrap();

    assert!(app.poll_file_launch());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::ConfigError {
            path: returned_path,
            ..
        } if returned_path == path
    ));
}

#[test]
fn protected_system_volume_never_opens_an_action_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Devices(DeviceView {
        devices: vec![LuksDevice {
            source: PathBuf::from("/dev/system-root"),
            drive: PathBuf::from("/dev/system-root"),
            label: Some("System".into()),
            size: 1,
            mapping: Some(PathBuf::from("/dev/mapper/system-root")),
            mountpoints: vec![PathBuf::from("/")],
            system_protected: true,
            ejectable: false,
            eject_blocked: false,
            kind: "part".into(),
            filesystem: Some("crypto_LUKS".into()),
            encrypted: true,
        }],
        selected: 0,
    });

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, AppMode::Devices(_)));
    assert!(app.luks_operation.is_none());
    assert!(app.status.contains("protected system device"));
}

#[test]
fn notices_expire_but_later_errors_remain_visible() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.set_notice("Mounted /dev/test");
    app.status_expiry = Some((app.status.clone(), Instant::now() - Duration::from_secs(1)));

    assert_eq!(app.visible_status(), "");

    app.status = "Mount failed".into();
    assert_eq!(app.visible_status(), "Mount failed");
}

#[test]
fn removable_device_offers_state_aware_eject_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Devices(DeviceView {
        devices: vec![LuksDevice {
            source: PathBuf::from("/dev/sdb1"),
            drive: PathBuf::from("/dev/sdb"),
            label: Some("Vault".into()),
            size: 1,
            mapping: None,
            mountpoints: Vec::new(),
            system_protected: false,
            ejectable: true,
            eject_blocked: false,
            kind: "part".into(),
            filesystem: Some("crypto_LUKS".into()),
            encrypted: true,
        }],
        selected: 0,
    });

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

    assert!(matches!(
        &app.mode,
        AppMode::Prompt(Prompt::ConfirmLuks {
            action: LuksAction::Eject { source, drive },
            body,
            ..
        }) if source == Path::new("/dev/sdb1")
            && drive == Path::new("/dev/sdb")
            && body.contains("safely ejected")
            && !body.contains("unmounted")
    ));
}

#[test]
fn device_progress_tracks_phase_and_total_duration() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let (sender, receiver) = mpsc::channel();
    let started_at = Instant::now() - Duration::from_millis(1_250);
    app.mode = AppMode::Progress;
    app.progress = ProgressState {
        label: "Safely ejecting device".into(),
        phase: Some("Preparing device operation".into()),
        started_at: Some(started_at),
        phase_started_at: Some(started_at),
        ..ProgressState::default()
    };
    app.luks_operation = Some(RunningLuks {
        receiver,
        retry: None,
        started_at,
    });

    let phase_started_at = Instant::now() - Duration::from_millis(250);
    sender
        .send(LuksUpdate::Phase {
            label: "Ejecting device",
            started_at: phase_started_at,
        })
        .unwrap();
    assert!(app.poll_luks_operation());
    assert_eq!(app.progress.phase.as_deref(), Some("Ejecting device"));
    assert_eq!(app.progress.phase_started_at, Some(phase_started_at));

    sender
        .send(LuksUpdate::Finished(Ok(LuksOutcome {
            message: "Safely ejected /dev/test".into(),
            mountpoint: Some(temp.path().to_path_buf()),
        })))
        .unwrap();
    assert!(app.poll_luks_operation());
    assert!(app.status.contains("Safely ejected /dev/test · took 1.2s"));
    assert!(matches!(app.mode, AppMode::Prompt(Prompt::Mounted { .. })));
}

#[test]
fn elapsed_duration_has_stable_tenths_and_minutes() {
    assert_eq!(format_elapsed(Duration::from_millis(80)), "0.0s");
    assert_eq!(format_elapsed(Duration::from_millis(1_290)), "1.2s");
    assert_eq!(format_elapsed(Duration::from_millis(64_320)), "1m 4.3s");
}

#[test]
fn eject_is_not_offered_when_another_volume_is_active() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Devices(DeviceView {
        devices: vec![LuksDevice {
            source: PathBuf::from("/dev/sdb1"),
            drive: PathBuf::from("/dev/sdb"),
            label: None,
            size: 1,
            mapping: None,
            mountpoints: Vec::new(),
            system_protected: false,
            ejectable: true,
            eject_blocked: true,
            kind: "part".into(),
            filesystem: Some("crypto_LUKS".into()),
            encrypted: true,
        }],
        selected: 0,
    });

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

    assert!(matches!(app.mode, AppMode::Devices(_)));
    assert!(app.luks_operation.is_none());
}

fn trash_view_with_entries(root: &Path, count: usize) -> TrashView {
    let manager = TrashManager::isolated(root);
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    for index in 0..count {
        let source = workspace.join(format!("item-{index}"));
        std::fs::write(&source, b"safe test data").unwrap();
        manager
            .move_to_trash(
                &source,
                &root.join("unrelated-current-directory"),
                &root.join("config"),
            )
            .unwrap();
    }
    TrashView {
        entries: manager.list().unwrap(),
        manager,
        selected: 0,
        marked: HashSet::new(),
    }
}

#[test]
fn trash_selection_and_confirmed_delete_target_marked_items() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Trash(trash_view_with_entries(temp.path(), 2));

    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    assert!(matches!(
        &app.mode,
        AppMode::Prompt(Prompt::ConfirmPermanentDelete {
            entries,
            clear_all: false,
            ..
        }) if entries.len() == 1
    ));
}

#[test]
fn clear_trash_always_opens_confirmation_for_every_item() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Trash(trash_view_with_entries(temp.path(), 3));

    app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));

    assert!(matches!(
        &app.mode,
        AppMode::Prompt(Prompt::ConfirmPermanentDelete {
            entries,
            clear_all: true,
            ..
        }) if entries.len() == 3
    ));
}

#[test]
fn quick_permanent_delete_skips_the_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let view = trash_view_with_entries(temp.path(), 1);
    let trashed_path = view.entries[0].trashed_path.clone();
    app.mode = AppMode::Trash(view);

    app.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Progress));

    for _ in 0..100 {
        app.poll_operation();
        if app.operation.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.operation.is_none());
    assert!(!trashed_path.exists());
}

#[test]
fn restoring_from_trash_refreshes_the_browser_entries() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let source = workspace.join("restored.txt");
    std::fs::write(&source, b"restored bytes").unwrap();
    let manager = TrashManager::isolated(temp.path());
    let entry = manager
        .move_to_trash(
            &source,
            &temp.path().join("unrelated-current-directory"),
            &temp.path().join("config"),
        )
        .unwrap();
    let mut app = test_app(&workspace);
    app.mode = AppMode::Prompt(Prompt::ConfirmRestore {
        entries: vec![entry],
        manager,
    });

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(source.exists());
    assert!(app.entries.iter().any(|entry| entry.path == source));
    assert!(matches!(app.mode, AppMode::Trash(_)));
}

#[test]
fn rename_cursor_supports_navigation_and_editing() {
    let mut input = "alpha.txt".to_string();
    let mut cursor = input.chars().count();

    edit_cursor_input(
        &mut input,
        &mut cursor,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
    );
    edit_cursor_input(
        &mut input,
        &mut cursor,
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
    );
    assert_eq!(input, "alpha.tx!t");

    edit_cursor_input(
        &mut input,
        &mut cursor,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    assert_eq!(input, "alpha.txt");
}

#[test]
fn rename_handles_files_and_directories() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("old-file");
    let directory = temp.path().join("old-directory");
    std::fs::write(&file, b"safe").unwrap();
    std::fs::create_dir(&directory).unwrap();
    let mut app = test_app(temp.path());

    app.rename(&file, "new-file");
    app.rename(&directory, "new-directory");

    assert!(temp.path().join("new-file").is_file());
    assert!(temp.path().join("new-directory").is_dir());
}

#[test]
fn create_file_uses_the_prompt_and_selects_the_new_empty_file() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    for character in "notes.txt".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let created = temp.path().join("notes.txt");
    assert_eq!(std::fs::metadata(&created).unwrap().len(), 0);
    assert_eq!(
        app.selected_entry().map(|entry| &entry.path),
        Some(&created)
    );
    assert!(matches!(app.mode, AppMode::Browser));
}

#[test]
fn create_file_never_overwrites_an_existing_entry() {
    let temp = tempfile::tempdir().unwrap();
    let existing = temp.path().join("existing.txt");
    std::fs::write(&existing, b"keep these bytes").unwrap();
    let mut app = test_app(temp.path());

    let mode = app.create_file("existing.txt");

    assert_eq!(std::fs::read(&existing).unwrap(), b"keep these bytes");
    assert!(matches!(
        mode,
        AppMode::Prompt(Prompt::Message { title, .. }) if title == "File already exists"
    ));
}

#[test]
fn create_file_rejects_nested_or_special_names() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());

    for name in ["", ".", "..", "nested/file"] {
        assert!(matches!(
            app.create_file(name),
            AppMode::Prompt(Prompt::Message { title, .. }) if title == "Invalid file name"
        ));
    }
    assert!(!temp.path().join("nested").exists());
}

#[test]
fn create_file_refuses_an_existing_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.txt");
    let link = temp.path().join("link.txt");
    std::fs::write(&target, b"safe target").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut app = test_app(temp.path());

    let mode = app.create_file("link.txt");

    assert_eq!(std::fs::read(&target).unwrap(), b"safe target");
    assert!(matches!(
        mode,
        AppMode::Prompt(Prompt::Message { title, .. }) if title == "File already exists"
    ));
}

#[test]
fn parent_navigation_uses_left_or_h_but_not_backspace() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let child = child.canonicalize().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    let mut app = test_app(&child);

    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.current_dir, child);

    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.current_dir, parent);
}

#[test]
fn tree_is_the_default_browser_view() {
    let temp = tempfile::tempdir().unwrap();
    let app = test_app(temp.path());

    assert_eq!(app.browser_view, BrowserView::Tree);
    assert_eq!(app.browser_view_label(), "Tree");
}

#[test]
fn tree_navigation_expands_descends_collapses_and_returns_to_parent() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    let grandchild = child.join("grandchild");
    let nested_file = grandchild.join("nested.txt");
    std::fs::create_dir_all(&grandchild).unwrap();
    std::fs::write(&nested_file, b"nested").unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.is_tree_directory_expanded(&child));
    assert_eq!(app.selected_entry().unwrap().path, child);
    assert_eq!(app.tree_depths, [0, 1]);

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.selected_entry().unwrap().path, grandchild);
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.is_tree_directory_expanded(&grandchild));
    assert_eq!(app.tree_depths, [0, 1, 2]);

    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(!app.is_tree_directory_expanded(&grandchild));
    assert_eq!(app.selected_entry().unwrap().path, grandchild);
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert_eq!(app.selected_entry().unwrap().path, child);
}

#[test]
fn view_toggle_preserves_a_nested_selector_and_table_navigation() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    let file = child.join("notes.txt");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(&file, b"notes").unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.selected_entry().unwrap().path, file);

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert_eq!(app.browser_view, BrowserView::Table);
    assert_eq!(app.current_dir, child);
    assert_eq!(app.selected_entry().unwrap().path, file);
    assert!(app.tree_depths.is_empty());

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert_eq!(app.browser_view, BrowserView::Tree);
    assert_eq!(app.current_dir, child);
    assert_eq!(app.selected_entry().unwrap().path, file);

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert_eq!(app.current_dir, temp.path());
    assert_eq!(app.selected_entry().unwrap().path, child);
}

#[test]
fn terminal_editor_restores_nested_tree_selector_and_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    let file = child.join("notes.txt");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(&file, b"before").unwrap();
    let mut app = test_app(temp.path());
    app.config.open.editor = "nano".into();

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let action = app.take_terminal_editor().expect("terminal editor queued");
    std::fs::write(&file, b"after").unwrap();
    app.finish_terminal_editor(&action, Ok(()));

    assert!(app.is_tree_directory_expanded(&child));
    assert_eq!(app.selected_entry().unwrap().path, file);
    assert_eq!(app.tree_depth(app.cursor), 1);
}

#[test]
fn tree_never_expands_a_directory_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("secret.txt"), b"safe").unwrap();
    symlink(&target, &link).unwrap();
    let mut app = test_app(temp.path());
    app.expanded_directories.insert(link.clone());

    app.refresh();

    assert!(!app
        .entries
        .iter()
        .any(|entry| entry.path == link.join("secret.txt")));
    assert!(app.tree_depths.iter().all(|depth| *depth == 0));
}

#[test]
fn slash_opens_quick_search_and_empty_f_expands_it() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('/'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if !form.advanced));
    app.handle_key(key('F'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.advanced && form.draft.scope == SearchScope::CurrentDirectory));
}

#[test]
fn advanced_search_up_down_selects_sections_and_tab_stays_within_section() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 0));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 1));
    app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 0));
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Scope));
}

#[test]
fn advanced_search_cycles_choices_and_toggles_entry_kinds() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.scope == SearchScope::CurrentDirectory));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.name_mode == crate::search::NameMode::Glob));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if !form.draft.types.contains(crate::entry::EntryKind::Directory)));
    for _ in 0..9 {
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.include_ignored_hidden));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.result_limit == crate::search::ResultLimit::TenThousand));
}

#[test]
fn advanced_search_type_arrows_wrap_without_toggling() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.error = Some("keep this validation error".into());
    }

    let initial_types = match &app.mode {
        AppMode::SearchForm(form) => form.draft.types,
        _ => panic!("advanced search form was not open"),
    };
    for expected_field in [1, 2, 3, 4, 0] {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(&app.mode, AppMode::SearchForm(form)
                if form.field == expected_field
                    && form.draft.types == initial_types
                    && form.error.as_deref() == Some("keep this validation error")));
    }

    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.field == 4
                && form.draft.types == initial_types
                && form.error.as_deref() == Some("keep this validation error")));

    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.field == 4 && form.draft.types == crate::search::EntryKinds::OTHER));
}

#[test]
fn advanced_search_enter_validates_from_every_section_and_keeps_error_inline() {
    for section_moves in 0..4 {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        for _ in 0..section_moves {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.minimum_size = "not-a-size".into();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
                if form.error.as_deref().is_some_and(|error| error.contains("invalid minimum size"))));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.minimum_size.clear();
            form.draft.name = "needle".into();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchProgress));
        app.search
            .take()
            .unwrap()
            .cancel
            .store(true, Ordering::Relaxed);
    }
}

#[test]
fn invalid_content_regex_keeps_form_and_does_not_start_or_replace_search() {
    let temp = tempfile::tempdir().unwrap();
    let old_path = temp.path().join("old-result");
    std::fs::write(&old_path, []).unwrap();
    let mut app = test_app(temp.path());
    let mut old_draft = SearchDraft::quick(temp.path().to_path_buf());
    old_draft.name = "old".into();
    app.search_results = Some(SearchView {
        request: old_draft.compile(true).unwrap(),
        results: vec![search::hit_for_test(old_path.clone(), "old")],
        selected: 0,
        selected_path: Some(old_path.clone()),
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchResults;
    app.handle_key(key('F'));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.draft.content = "(".into();
        form.draft.content_mode = crate::search::ContentMode::Regex;
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.search.is_none());
    assert_eq!(
        app.search_results.as_ref().unwrap().results[0].entry.path,
        old_path
    );
    assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.draft.content == "(" && form.error.as_deref().is_some_and(|e| e.contains("invalid content regex"))));
}

#[test]
fn advanced_search_preserves_validation_error_while_navigating() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.draft.minimum_size = "bad".into();
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Up] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    }
}

#[test]
fn advanced_search_text_fields_keep_independent_unicode_cursors() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.draft.name = "a界b".into();
        form.cursors.name = 2;
    }
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for ch in "xy".chars() {
        app.handle_key(key(ch));
    }
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.handle_key(key('界'));
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    app.handle_key(key('Z'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.name == "a界Zb" && form.draft.content == "x界y"
                && form.cursors.name == 3 && form.cursors.content == 2));
}

#[test]
fn advanced_search_space_is_noop_for_non_type_choices() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    app.handle_key(key(' '));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.scope == SearchScope::Filesystem && form.draft.name.is_empty()));
}

#[test]
fn configured_filesystem_binding_expands_only_an_empty_quick_query() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.config =
        toml::from_str::<crate::config::Config>("[hotkeys]\nsearch_filesystem = 'G'").unwrap();
    app.handle_key(key('/'));
    app.handle_key(key('G'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.advanced));
    app.mode = AppMode::Browser;
    app.handle_key(key('/'));
    app.handle_key(key('x'));
    app.handle_key(key('G'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.name == "xG"));
}

#[test]
fn quick_search_error_clears_only_when_query_value_changes() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('/'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    for code in [KeyCode::Left, KeyCode::Home, KeyCode::Up] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    }
    app.handle_key(key('x'));
    assert!(
        matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none() && form.draft.name == "x")
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.search
        .take()
        .unwrap()
        .cancel
        .store(true, Ordering::Relaxed);

    app.mode = AppMode::Browser;
    app.handle_key(key('/'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(
        matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none() && form.draft.name.is_empty())
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
}

#[test]
fn advanced_search_scope_cycles_from_exact_initial_scope() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    assert!(
        matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::Filesystem)
    );
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(
        matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::CurrentDirectory)
    );
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::Filesystem)
    );
}

#[test]
fn advanced_search_error_survives_caret_moves_and_noop_controls() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.draft.minimum_size = "bad".into();
        form.draft.content = "a界b".into();
        form.cursors.content = 2;
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    }
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
}

#[test]
fn advanced_search_error_clears_only_after_value_mutation() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('F'));
    if let AppMode::SearchForm(form) = &mut app.mode {
        form.draft.minimum_size = "bad".into();
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none()));
}

#[test]
fn typed_f_is_query_input_and_global_f_uses_filesystem_scope() {
    let mut app = test_app(tempfile::tempdir().unwrap().path());
    app.handle_key(key('/'));
    app.handle_key(key('x'));
    app.handle_key(key('F'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.name == "xF"));
    app.mode = AppMode::Browser;
    app.handle_key(key('F'));
    assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.advanced && form.draft.scope == SearchScope::Filesystem));
}

#[test]
fn search_typing_does_not_start_worker_and_enter_starts_once() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("needle.txt"), b"needle").unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(key('/'));
    for character in "needle".chars() {
        app.handle_key(key(character));
        assert!(app.search.is_none());
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.search.is_some());
    let cancel = Arc::clone(&app.search.as_ref().unwrap().cancel);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(Arc::ptr_eq(&cancel, &app.search.as_ref().unwrap().cancel));
    wait_for_search(&mut app);
}

#[test]
fn search_results_preserve_browser_state_and_old_results_on_form_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let target = child.join("needle.txt");
    std::fs::write(&target, b"needle").unwrap();
    let mut app = test_app(temp.path());
    app.expanded_directories.insert(child.clone());
    app.refresh();
    app.cursor = app
        .entries
        .iter()
        .position(|entry| entry.path == target)
        .unwrap();
    app.entries[app.cursor].selected = true;
    let cursor_path = app.selected_entry().unwrap().path.clone();
    let marked = app
        .entries
        .iter()
        .filter(|entry| entry.selected)
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();

    app.handle_key(key('/'));
    for character in "needle".chars() {
        app.handle_key(key(character));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_search(&mut app);
    assert!(matches!(app.mode, AppMode::SearchResults));
    let result_count = app.search_results.as_ref().unwrap().results.len();
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.current_dir, temp.path());
    assert_eq!(app.selected_entry().unwrap().path, cursor_path);
    assert!(app.expanded_directories.contains(&child));
    assert_eq!(
        app.entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
        marked
    );

    app.mode = AppMode::SearchResults;
    app.handle_key(key('/'));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert_eq!(
        app.search_results.as_ref().unwrap().results.len(),
        result_count
    );
    assert_eq!(app.selected_entry().unwrap().path, cursor_path);
}

#[test]
fn poll_search_is_bounded_to_updates_per_tick() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..UPDATES_PER_UI_TICK + 20 {
        std::fs::write(temp.path().join(format!("needle-{index}.txt")), b"").unwrap();
    }
    let mut app = test_app(temp.path());
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "needle".into();
    let request = draft.compile(true).unwrap();
    app.search_results = Some(SearchView {
        request,
        results: Vec::new(),
        selected: 0,
        selected_path: None,
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchProgress;
    let updates = (0..UPDATES_PER_UI_TICK + 20)
        .map(|index| {
            SearchUpdate::Match(search::hit_for_test(
                temp.path().join(format!("needle-{index}.txt")),
                "needle",
            ))
        })
        .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
        .collect();
    app.search = Some(search::running_search_for_test(updates));

    assert!(app.poll_search());
    assert_eq!(app.search_matches, UPDATES_PER_UI_TICK);
    assert!(app.search.is_some());
    assert!(matches!(app.mode, AppMode::SearchProgress));
}

fn poll_synthetic_hits(count: usize) -> (App, Duration) {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "item".into();
    app.search_results = Some(SearchView {
        request: draft.compile(true).unwrap(),
        results: Vec::new(),
        selected: 0,
        selected_path: None,
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    let updates = (0..count)
        .rev()
        .map(|index| {
            let name = format!("item-{index:05}-Straße");
            SearchUpdate::Match(search::synthetic_hit_for_test(
                PathBuf::from("/benchmark").join(&name),
                name,
            ))
        })
        .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
        .collect();
    app.search = Some(search::running_search_for_test(updates));
    app.mode = AppMode::SearchProgress;
    search::reset_case_fold_calls_for_test();
    let started = Instant::now();
    while app.search.is_some() {
        app.poll_search();
    }
    (app, started.elapsed())
}

#[test]
fn poll_search_sorts_ten_thousand_streamed_hits_without_comparator_folding() {
    let (app, _) = poll_synthetic_hits(10_000);
    let results = &app.search_results.as_ref().unwrap().results;
    assert_eq!(results.len(), 10_000);
    assert!(results.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(search::case_fold_calls_for_test(), 0);
    assert_eq!(search::sort_key_allocations_for_test(), 0);
}

#[test]
#[ignore]
fn benchmark_poll_search_stream_sort_ten_thousand() {
    let mut samples = Vec::new();
    for _ in 0..9 {
        let (_, elapsed) = poll_synthetic_hits(10_000);
        assert_eq!(search::case_fold_calls_for_test(), 0);
        assert_eq!(search::sort_key_allocations_for_test(), 0);
        samples.push(elapsed.as_micros());
    }
    samples.sort_unstable();
    eprintln!(
        "PERF app_search_sort hits=10000 median_us={} comparator_case_folds=0",
        samples[samples.len() / 2]
    );
}

#[test]
fn poll_search_adds_aggregate_skipped_count() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "needle".into();
    app.search_results = Some(SearchView {
        request: draft.compile(true).unwrap(),
        results: Vec::new(),
        selected: 0,
        selected_path: None,
        skipped: 2,
        truncated: false,
        incomplete: false,
    });
    app.search = Some(search::running_search_for_test(vec![
        SearchUpdate::Skipped(7),
        SearchUpdate::Finished(Default::default()),
    ]));

    assert!(app.poll_search());
    assert_eq!(app.search_skipped, 9);
    assert_eq!(app.search_results.as_ref().unwrap().skipped, 9);
}

#[test]
fn cancelled_search_returns_to_results_without_clearing_them() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("needle.txt"), b"needle").unwrap();
    let mut app = test_app(temp.path());
    app.handle_key(key('/'));
    for character in "needle".chars() {
        app.handle_key(key(character));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_search(&mut app);
    let old_count = app.search_results.as_ref().unwrap().results.len();
    app.mode = AppMode::SearchResults;
    app.handle_key(key('/'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.previous_search_results.is_some());
    app.search = Some(search::running_search_for_test(vec![
        SearchUpdate::Finished(search::SearchCompletion {
            cancelled: true,
            truncated: false,
            incomplete: false,
        }),
    ]));

    assert!(app.poll_search());
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert_eq!(
        app.search_results.as_ref().unwrap().results.len(),
        old_count
    );
}

#[test]
fn streamed_hits_sort_and_keep_selected_path() {
    let temp = tempfile::tempdir().unwrap();
    let selected = temp.path().join("needle-z.txt");
    let better = temp.path().join("needle.txt");
    std::fs::write(&selected, b"").unwrap();
    std::fs::write(&better, b"").unwrap();
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "needle".into();
    let request = draft.compile(true).unwrap();
    let selected_hit = search::hit_for_test(selected.clone(), "needle");
    let better_hit = search::hit_for_test(better.clone(), "needle");
    let mut app = test_app(temp.path());
    app.search_results = Some(SearchView {
        request,
        results: vec![selected_hit],
        selected: 0,
        selected_path: Some(selected.clone()),
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchProgress;
    app.search = Some(search::running_search_for_test(vec![
        SearchUpdate::Match(better_hit),
        SearchUpdate::Finished(Default::default()),
    ]));

    assert!(app.poll_search());
    let view = app.search_results.as_ref().unwrap();
    assert_eq!(view.results[0].entry.path, better);
    assert_eq!(view.results[view.selected].entry.path, selected);
    assert_eq!(view.selected_path.as_deref(), Some(selected.as_path()));
}

#[test]
fn mixed_name_hits_use_production_insertion_order_and_keep_selected_path() {
    let temp = tempfile::tempdir().unwrap();
    let path = |bytes: &[u8], group: &str| {
        temp.path()
            .join(group)
            .join(OsString::from_vec(bytes.to_vec()))
    };
    let selected = path(b"Z", "selected");
    let paths = [
        selected.clone(),
        path(b"a", "valid"),
        path(&[0x60, 0x80], "invalid-0"),
        path(b"Alpha", "collision-0"),
        path(b"alpha", "collision-1"),
        path("Straße".as_bytes(), "unicode-0"),
        path(b"STRASSE", "unicode-1"),
        path(&[0x61, 0xff], "invalid-1"),
    ];
    let hits = paths
        .iter()
        .map(|path| search::synthetic_hit_for_test(path.clone(), "hit".into()))
        .collect::<Vec<_>>();
    let mut expected = hits.clone();
    expected.sort();

    let mut inserted = Vec::new();
    for index in [2, 6, 0, 7, 3, 1, 5, 4] {
        insert_search_hit(&mut inserted, hits[index].clone());
    }
    assert_eq!(
        inserted
            .iter()
            .map(|hit| &hit.entry.path)
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|hit| &hit.entry.path)
            .collect::<Vec<_>>()
    );

    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "hit".into();
    let mut app = test_app(temp.path());
    app.search_results = Some(SearchView {
        request: draft.compile(true).unwrap(),
        results: vec![hits[0].clone()],
        selected: 0,
        selected_path: Some(selected.clone()),
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchProgress;
    app.search = Some(search::running_search_for_test(
        [2, 6, 7, 3, 1, 5, 4]
            .into_iter()
            .map(|index| SearchUpdate::Match(hits[index].clone()))
            .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
            .collect(),
    ));
    assert!(app.poll_search());
    let view = app.search_results.as_ref().unwrap();
    assert_eq!(
        view.results
            .iter()
            .map(|hit| &hit.entry.path)
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|hit| &hit.entry.path)
            .collect::<Vec<_>>()
    );
    assert_eq!(view.results[view.selected].entry.path, selected);
    assert_eq!(view.selected_path.as_deref(), Some(selected.as_path()));
}

#[test]
fn disconnected_search_preserves_hits_and_marks_results_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("needle.txt");
    std::fs::write(&path, b"").unwrap();
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.name = "needle".into();
    let request = draft.compile(true).unwrap();
    let hit = search::hit_for_test(path.clone(), "needle");
    let mut app = test_app(temp.path());
    app.search_results = Some(SearchView {
        request,
        results: Vec::new(),
        selected: 0,
        selected_path: None,
        skipped: 0,
        truncated: false,
        incomplete: false,
    });
    app.mode = AppMode::SearchProgress;
    app.search = Some(search::running_search_for_test(vec![SearchUpdate::Match(
        hit,
    )]));

    assert!(app.poll_search());
    assert!(matches!(app.mode, AppMode::SearchResults));
    assert!(app.search.is_none());
    let view = app.search_results.as_ref().unwrap();
    assert_eq!(view.results[0].entry.path, path);
    assert!(view.incomplete);
    assert_eq!(app.status, "Search worker stopped unexpectedly");
}

#[test]
fn disconnected_cancelling_search_restores_prior_browser_results() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("needle.txt"), b"").unwrap();
    let mut app = test_app(temp.path());
    app.handle_key(key('/'));
    for character in "needle".chars() {
        app.handle_key(key(character));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_search(&mut app);
    let old_paths = app
        .search_results
        .as_ref()
        .unwrap()
        .results
        .iter()
        .map(|hit| hit.entry.path.clone())
        .collect::<Vec<_>>();
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.handle_key(key('/'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.search_cancelling = true;
    app.search = Some(search::running_search_for_test(Vec::new()));

    assert!(app.poll_search());
    assert!(matches!(app.mode, AppMode::Browser));
    assert!(app.search.is_none());
    assert_eq!(
        app.search_results
            .as_ref()
            .unwrap()
            .results
            .iter()
            .map(|hit| hit.entry.path.clone())
            .collect::<Vec<_>>(),
        old_paths
    );
}

#[test]
fn browser_search_cancellation_restores_existing_results() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("needle.txt"), b"").unwrap();
    let mut app = test_app(temp.path());
    app.handle_key(key('/'));
    for character in "needle".chars() {
        app.handle_key(key(character));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_search(&mut app);
    let old_count = app.search_results.as_ref().unwrap().results.len();
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.handle_key(key('/'));
    app.handle_key(key('x'));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.search = Some(search::running_search_for_test(vec![
        SearchUpdate::Finished(search::SearchCompletion {
            cancelled: true,
            ..Default::default()
        }),
    ]));

    assert!(app.poll_search());
    assert!(matches!(app.mode, AppMode::Browser));
    assert_eq!(
        app.search_results.as_ref().unwrap().results.len(),
        old_count
    );
}

#[test]
fn background_refresh_keeps_only_the_latest_request() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..2_000 {
        std::fs::write(temp.path().join(format!("noise-{index:04}")), []).unwrap();
    }
    let target = temp.path().join("target.txt");
    std::fs::write(&target, b"target").unwrap();
    let mut app = test_app(temp.path());

    app.request_browser_load(None);
    app.request_browser_load(Some(target.clone()));

    let deadline = Instant::now() + Duration::from_secs(5);
    while app.browser_loading && Instant::now() < deadline {
        app.poll_browser_load();
        thread::sleep(Duration::from_millis(1));
    }

    assert!(!app.browser_loading);
    assert!(app.entries.iter().any(|entry| entry.path == target));
    assert_eq!(app.selected_entry().unwrap().path, target);
    assert!(app.pending_browser_load.is_none());
}

#[test]
fn background_refresh_preserves_navigation_during_streaming() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..2_000 {
        std::fs::write(temp.path().join(format!("item-{index:04}")), []).unwrap();
    }
    let mut app = test_app(temp.path());
    app.request_browser_load(None);

    let deadline = Instant::now() + Duration::from_secs(5);
    while app.entries.len() < 10 && Instant::now() < deadline {
        app.poll_browser_load();
        thread::sleep(Duration::from_millis(1));
    }
    app.move_cursor(5);
    let selected_while_loading = app.selected_entry().unwrap().path.clone();
    while app.browser_loading && Instant::now() < deadline {
        app.poll_browser_load();
        thread::sleep(Duration::from_millis(1));
    }

    assert!(!app.browser_loading);
    assert_eq!(app.selected_entry().unwrap().path, selected_while_loading);
}

#[test]
fn disconnected_browser_worker_clears_loading_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let (sender, receiver) = mpsc::sync_channel::<LoadUpdate>(1);
    drop(sender);
    app.browser_generation = 9;
    app.browser_loading = true;
    app.browser_load = Some(RunningLoad {
        generation: 9,
        receiver,
        cancel: Arc::new(AtomicBool::new(false)),
    });

    assert!(app.poll_browser_load());
    assert!(!app.browser_loading);
    assert_eq!(app.status, "directory load worker stopped unexpectedly");
}

#[test]
#[ignore]
fn benchmark_background_device_discovery() {
    let mut synchronous_samples = Vec::new();
    let mut enqueue_samples = Vec::new();
    let mut background_samples = Vec::new();
    for _ in 0..9 {
        let synchronous_started = Instant::now();
        let _ = luks::discover();
        synchronous_samples.push(synchronous_started.elapsed());

        let started = Instant::now();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(luks::discover());
        });
        enqueue_samples.push(started.elapsed());
        let _ = receiver.recv().unwrap();
        background_samples.push(started.elapsed());
    }
    synchronous_samples.sort();
    enqueue_samples.sort();
    background_samples.sort();
    println!(
        "PERF device_sync_median_us={} device_enqueue_median_us={} device_background_median_us={}",
        synchronous_samples[4].as_micros(),
        enqueue_samples[4].as_micros(),
        background_samples[4].as_micros(),
    );
}

#[test]
fn u_does_not_open_the_disk_manager() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
}

#[test]
fn uppercase_n_opens_the_separate_network_share_manager() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.network_environment = test_network_environment(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));

    assert!(matches!(app.mode, AppMode::Network(_)));
    assert!(app.network_refreshing);
    for _ in 0..100 {
        app.poll_network();
        if !app.network_refreshing {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(app.mode, AppMode::Network(_)));
    assert!(!app.network_refreshing);
}

#[test]
fn network_share_prompt_owns_input_and_supports_cursor_editing() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.network_environment = test_network_environment(temp.path());
    app.mode = AppMode::Network(NetworkView {
        shares: Vec::new(),
        selected: 0,
    });

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    for character in "nas/share".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::SmbUsername { ref address, .. })
            if address.uri == "smb://nas/share"
    ));
    assert!(app.operation.is_none());
}

#[test]
fn read_only_network_manager_can_open_connected_share_but_not_change_it() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mounted");
    std::fs::create_dir(&mount).unwrap();
    let mut app = test_app(temp.path());
    app.config.behavior.read_only = true;
    let share = NetworkShare {
        address: ShareAddress::parse("smb://nas/public").unwrap(),
        mount_path: Some(mount.clone()),
        username: None,
        domain: None,
        saved: false,
        discovered: true,
    };
    app.mode = AppMode::Network(NetworkView {
        shares: vec![share],
        selected: 0,
    });

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Network(_)));
    assert!(app.network_operation.is_none());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
    assert_eq!(app.current_dir, mount);
}

#[test]
fn uppercase_m_opens_and_navigates_the_apps_launcher() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT));
    assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 1 })));

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
}

#[test]
fn configured_app_hotkey_replaces_the_default() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.config = toml::from_str("[hotkeys]\ntools = 'F2'\n").unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT));
    assert!(matches!(app.mode, AppMode::Browser));

    app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));
}

#[test]
fn one_device_hotkey_opens_the_unified_manager() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());

    app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Partitions(_)));

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Browser));
}

#[test]
fn partition_view_navigation_is_modal_and_returns_to_apps() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("untouched.txt");
    std::fs::write(&file, b"safe").unwrap();
    let mut app = test_app(temp.path());
    app.partition_return_to_apps = true;
    app.mode = AppMode::Partitions(PartitionView {
        entries: Vec::new(),
        selected: 0,
        overlay: None,
    });

    for key in [KeyCode::Char('d'), KeyCode::Char('x'), KeyCode::Enter] {
        app.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
    }
    assert!(file.exists());
    assert!(matches!(app.mode, AppMode::Partitions(_)));

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));
}

#[test]
fn read_only_mode_opens_device_manager_for_safe_inspection() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = App::new(
        temp.path().to_path_buf(),
        ConfigLoad::Valid {
            config: Config::default(),
            path: temp.path().join("config.toml"),
        },
        true,
    );
    app.mode = AppMode::Apps(AppsView { selected: 0 });

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, AppMode::Partitions(_)));
}

fn partition_test_view() -> PartitionView {
    let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:16\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" LABEL=\"Data\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n",
        );
    PartitionView {
        entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
        selected: 1,
        overlay: None,
    }
}

#[test]
fn partition_format_flow_chooses_a_filesystem_and_optional_label() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Partitions(partition_test_view());

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::FormatOptions { selected: 0, .. }),
            ..
        })
    ));

    for _ in 0..7 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::FormatLabel {
                filesystem: Filesystem::Exfat,
                ..
            }),
            ..
        })
    ));

    for character in "Archive".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm {
                action: PartitionAction::Format {
                    filesystem: Filesystem::Exfat,
                    label: Some(ref label),
                    ..
                },
                yes_selected: false,
            }),
            ..
        }) if label == "Archive"
    ));

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm {
                yes_selected: true,
                ..
            }),
            ..
        })
    ));

    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Actions { selected: 0 }),
            ..
        })
    ));
    assert!(app.partition_operation.is_none());
}

#[test]
fn partition_format_flow_offers_luks2_and_confirms_the_passphrase_twice() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.mode = AppMode::Partitions(partition_test_view());

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::FormatLabel {
                encrypted: true,
                ..
            }),
            ..
        })
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for character in "correct horse".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for character in "correct horse".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm {
                action: PartitionAction::EncryptFormat {
                    filesystem: Filesystem::Ext4,
                    ..
                },
                yes_selected: false,
            }),
            ..
        })
    ));
}

#[test]
fn luks_passphrase_change_uses_three_masked_steps() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let fixture = "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"crypto_LUKS\" UUID=\"luks-id\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n";
    let view = PartitionView {
        entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
        selected: 0,
        overlay: None,
    };
    app.mode = app.begin_partition_task(view, PartitionTask::ChangePassphrase);
    for text in ["current key", "replacement key", "replacement key"] {
        for character in text.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
    let AppMode::Partitions(PartitionView {
        overlay: Some(PartitionOverlay::Confirm { action, .. }),
        ..
    }) = &app.mode
    else {
        panic!("expected passphrase-change confirmation");
    };
    assert!(matches!(
        action,
        PartitionAction::ChangeLuksPassphrase { .. }
    ));
    let rendered = format!("{action:?}");
    assert!(!rendered.contains("current key"));
    assert!(!rendered.contains("replacement key"));
}

#[test]
fn locked_luks_volume_offers_access_passphrase_and_encryption_options() {
    let temp = tempfile::tempdir().unwrap();
    let app = test_app(temp.path());
    let fixture = "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"crypto_LUKS\" UUID=\"luks-id\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n";
    let view = PartitionView {
        entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
        selected: 0,
        overlay: None,
    };
    let tasks = app.partition_tasks_for_view(&view);
    assert!(tasks.contains(&PartitionTask::EncryptionAccess));
    assert!(tasks.contains(&PartitionTask::ChangePassphrase));
    assert!(tasks.contains(&PartitionTask::EncryptionOptions));
    assert!(!tasks.contains(&PartitionTask::MountOptions));
}

#[test]
fn partition_tasks_are_context_sensitive_and_read_only_aware() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut view = partition_test_view();
    let partition_tasks = app.partition_tasks_for_view(&view);
    assert_eq!(
        partition_tasks,
        vec![
            PartitionTask::Mount,
            PartitionTask::Resize,
            PartitionTask::Format,
            PartitionTask::Delete,
            PartitionTask::Label,
            PartitionTask::Check,
            PartitionTask::Repair,
            PartitionTask::CreateImage,
            PartitionTask::RestoreImage,
            PartitionTask::PartitionName,
            PartitionTask::PartitionType,
            PartitionTask::Flag,
            PartitionTask::MountOptions,
        ]
    );
    assert!(!partition_tasks.contains(&PartitionTask::CreateTable));
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::Format)
        .is_none());
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::CreatePartition)
        .unwrap()
        .contains("whole disk"));

    view.selected = 0;
    let disk_tasks = app.partition_tasks_for_view(&view);
    assert_eq!(
        disk_tasks,
        vec![
            PartitionTask::CreatePartition,
            PartitionTask::CreateTable,
            PartitionTask::CreateImage,
            PartitionTask::RestoreImage,
            PartitionTask::SmartReport,
            PartitionTask::SmartShortTest,
            PartitionTask::SmartExtendedTest,
            PartitionTask::DriveSettings,
            PartitionTask::Eject,
        ]
    );
    assert_eq!(
        app.partition_task_name(&view, PartitionTask::CreateTable),
        "Format disk"
    );
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::CreatePartition)
        .is_none());

    view.entries[0].device.table_type = None;
    let blank_disk_tasks = app.partition_tasks_for_view(&view);
    assert_eq!(
        blank_disk_tasks,
        vec![
            PartitionTask::CreateTable,
            PartitionTask::Format,
            PartitionTask::CreateImage,
            PartitionTask::RestoreImage,
            PartitionTask::SmartReport,
            PartitionTask::SmartShortTest,
            PartitionTask::SmartExtendedTest,
            PartitionTask::DriveSettings,
            PartitionTask::Eject
        ]
    );
    assert_eq!(
        app.partition_task_name(&view, PartitionTask::CreateTable),
        "Format disk"
    );
    assert_eq!(
        app.partition_task_name(&view, PartitionTask::Format),
        "Use whole disk"
    );

    app.config.behavior.read_only = true;
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::CreatePartition)
        .unwrap()
        .contains("Read-only"));
}

#[test]
fn common_partition_types_map_to_exact_gpt_and_mbr_ids() {
    let temp = tempfile::tempdir().unwrap();
    let app = test_app(temp.path());
    let mut view = partition_test_view();

    assert_eq!(
        app.partition_type_id(&view, "linux").unwrap(),
        "0fc63daf-8483-4772-8e79-3d69d8477de4"
    );
    assert!(app.partition_type_id(&view, "something vague").is_err());

    view.entries[0].device.table_type = Some("dos".into());
    assert_eq!(app.partition_type_id(&view, "data").unwrap(), "07");
    assert_eq!(app.partition_type_id(&view, "af").unwrap(), "af");
}

#[test]
fn read_only_mode_still_allows_checks_and_table_backups() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    app.config.behavior.read_only = true;
    let mut view = partition_test_view();

    assert!(app
        .partition_task_unavailable(&view, PartitionTask::Check)
        .is_none());
    view.selected = 0;
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::BackupTable)
        .is_none());
    assert!(app
        .partition_task_unavailable(&view, PartitionTask::CreateTable)
        .is_some());
}

#[test]
fn disk_reset_chooses_empty_gpt_or_mbr_without_free_form_input() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut view = partition_test_view();
    view.selected = 0;
    view.overlay = Some(PartitionOverlay::Actions { selected: 1 });
    app.mode = AppMode::Partitions(view);

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::DiskLayoutOptions { selected: 0, .. }),
            ..
        })
    ));

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm {
                action: PartitionAction::EraseDisk { .. },
                yes_selected: false,
            }),
            ..
        })
    ));

    let view = partition_test_view();
    let gpt = app.partition_disk_layout_action(
        &PartitionView {
            selected: 0,
            ..view.clone()
        },
        1,
        false,
    );
    let mbr = app.partition_disk_layout_action(
        &PartitionView {
            selected: 0,
            ..view
        },
        2,
        false,
    );
    assert!(matches!(
        gpt,
        Ok(PartitionAction::CreateTable {
            table: PartitionTable::Gpt,
            ..
        })
    ));
    assert!(matches!(
        mbr,
        Ok(PartitionAction::CreateTable {
            table: PartitionTable::Msdos,
            ..
        })
    ));
}

#[test]
fn create_partition_defaults_to_max_and_accepts_a_custom_size() {
    let temp = tempfile::tempdir().unwrap();
    let app = test_app(temp.path());
    let mut view = partition_test_view();
    view.selected = 0;
    let region = partition::largest_free_region(&view.entries[0], &view.entries).unwrap();

    let (default_input, _) = app.partition_task_input(&view, PartitionTask::CreatePartition);
    assert_eq!(default_input, "max");

    let maximum = app
        .partition_action_from_input(&view, PartitionTask::CreatePartition, "max")
        .unwrap();
    assert!(matches!(
        maximum,
        PartitionAction::CreatePartition {
            start_bytes,
            end_bytes,
            ..
        } if start_bytes == region.0 && end_bytes == region.1
    ));

    let half = app
        .partition_action_from_input(&view, PartitionTask::CreatePartition, "50%")
        .unwrap();
    assert!(matches!(
        half,
        PartitionAction::CreatePartition {
            start_bytes,
            end_bytes,
            ..
        } if start_bytes == region.0
            && end_bytes > start_bytes
            && end_bytes - start_bytes <= (region.1 - region.0) / 2
            && end_bytes.is_multiple_of(512)
    ));
}

#[test]
fn create_partition_flow_selects_free_space_before_size() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut view = partition_test_view();
    view.selected = 0;
    view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
    app.mode = AppMode::Partitions(view);

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::FreeRegionOptions { selected: 0 }),
            ..
        })
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::PartitionSize { ref input, .. }),
            ..
        }) if input == "max"
    ));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm {
                action: PartitionAction::CreatePartition { .. },
                yes_selected: false,
            }),
            ..
        })
    ));
}

#[test]
fn resize_action_grows_or_shrinks_ext4_by_final_size() {
    let temp = tempfile::tempdir().unwrap();
    let app = test_app(temp.path());
    let view = partition_test_view();

    let shrink = app.partition_resize_action(&view, "50MiB").unwrap();
    assert!(matches!(shrink, PartitionAction::Shrink { .. }));
    partition::validate_snapshot(&shrink, &view.entries).unwrap();

    let grow = app.partition_resize_action(&view, "max").unwrap();
    assert!(matches!(grow, PartitionAction::Grow { .. }));
    partition::validate_snapshot(&grow, &view.entries).unwrap();
}

#[test]
fn partition_confirmation_opens_masked_administrator_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let mut view = partition_test_view();
    let action = PartitionAction::Format {
        target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
        filesystem: Filesystem::Ext4,
        label: None,
    };
    if !partition::authentication_required(&action) {
        return;
    }
    view.overlay = Some(PartitionOverlay::Confirm {
        action,
        yes_selected: false,
    });
    app.mode = AppMode::Partitions(view);

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::PartitionAuthentication { .. })
    ));

    for character in "not-a-real-password".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::PartitionAuthentication { ref input, .. })
            if input.character_count() == "not-a-real-password".chars().count()
    ));

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, AppMode::Partitions(_)));
    assert!(app.partition_operation.is_none());
}

#[test]
fn failed_partition_authentication_returns_to_a_clean_masked_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let view = partition_test_view();
    let action = PartitionAction::Format {
        target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
        filesystem: Filesystem::Ext4,
        label: None,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(PartitionUpdate::Finished(Err(
            crate::error::MinfmError::IncorrectPassphrase,
        )))
        .unwrap();
    app.partition_return_view = Some(view);
    app.partition_operation = Some(RunningPartitionOperation {
        receiver,
        started_at: Instant::now(),
        action,
    });

    assert!(app.poll_partition_operation());
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::PartitionAuthentication {
            ref input,
            error: Some(ref error),
            ..
        }) if input.is_empty() && error.contains("failed")
    ));
    assert!(app.partition_operation.is_none());
}

#[test]
fn every_smart_action_opens_a_scrollable_report_and_returns_to_devices() {
    let temp = tempfile::tempdir().unwrap();
    for action_kind in 0..3 {
        let mut app = test_app(temp.path());
        let mut view = partition_test_view();
        view.selected = 0;
        let disk = DeviceIdentity::from_entry(&view.entries[0]).unwrap();
        let action = match action_kind {
            0 => PartitionAction::SmartReport { disk },
            1 => PartitionAction::SmartTest {
                disk,
                extended: false,
            },
            _ => PartitionAction::SmartTest {
                disk,
                extended: true,
            },
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let report = (1..=30)
            .map(|line| format!("SMART report line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        sender.send(PartitionUpdate::Finished(Ok(report))).unwrap();
        app.partition_return_view = Some(view);
        app.partition_operation = Some(RunningPartitionOperation {
            receiver,
            started_at: Instant::now(),
            action,
        });

        assert!(app.poll_partition_operation());
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::SmartReport {
                ref body,
                scroll: 0,
                ..
            }) if body.contains("SMART report line 30")
        ));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::SmartReport { scroll: 1, .. })
        ));
        for _ in 0..10 {
            app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::SmartReport {
                ref body,
                scroll,
                ..
            }) if scroll == smart_report_scroll_limit(body)
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Partitions(_)));
    }
}

#[test]
fn partition_failures_open_a_complete_error_dialog() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app(temp.path());
    let view = partition_test_view();
    let action = PartitionAction::Format {
        target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
        filesystem: Filesystem::Ext4,
        label: None,
    };
    let message = "mkfs.ext4 failed because the device became unavailable";
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(PartitionUpdate::Finished(Err(
            crate::error::MinfmError::Message(message.into()),
        )))
        .unwrap();
    app.partition_return_view = Some(view);
    app.partition_operation = Some(RunningPartitionOperation {
        receiver,
        started_at: Instant::now(),
        action,
    });

    assert!(app.poll_partition_operation());
    assert!(matches!(
        app.mode,
        AppMode::Prompt(Prompt::PartitionError { ref body, .. })
            if body.contains("Format")
                && body.contains("/dev/sdb1")
                && body.contains(message)
    ));
    assert_eq!(app.status, "Partition operation failed");
    assert!(app.partition_operation.is_none());
}
