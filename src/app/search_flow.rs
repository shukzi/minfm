use super::*;

impl App {
    pub(crate) fn restore_previous_search_results(&mut self) {
        if let Some(previous) = self.previous_search_results.take() {
            self.search_results = Some(previous);
        }
        self.search_matches = self
            .search_results
            .as_ref()
            .map_or(0, |view| view.results.len());
        self.search_skipped = self.search_results.as_ref().map_or(0, |view| view.skipped);
    }

    pub(crate) fn focused_search_entry(&self) -> Option<&FileEntry> {
        self.search_results
            .as_ref()
            .and_then(|view| focused_entry_from_hits(&view.results, view.selected))
    }

    pub(crate) fn search_target_paths(&self) -> Vec<PathBuf> {
        self.search_results
            .as_ref()
            .map(|view| target_paths_from_hits(&view.results, view.selected))
            .unwrap_or_default()
    }

    pub(crate) fn revalidated_search_entries(
        &mut self,
        snapshots: Vec<(PathBuf, EntryKind)>,
    ) -> Vec<FileEntry> {
        let mut missing = 0;
        let mut changed = 0;
        for (path, expected_kind) in &snapshots {
            match fs::symlink_metadata(path) {
                Ok(metadata) if FileEntry::kind_from_metadata(&metadata) == *expected_kind => {}
                Ok(_) => changed += 1,
                Err(_) => missing += 1,
            }
        }
        self.refresh_search_results(None);
        let valid = snapshots
            .iter()
            .filter_map(|(path, expected_kind)| {
                self.search_results
                    .as_ref()?
                    .results
                    .iter()
                    .find_map(|hit| {
                        (&hit.entry.path == path && hit.entry.kind == *expected_kind)
                            .then(|| hit.entry.clone())
                    })
            })
            .collect::<Vec<_>>();
        if changed > 0 {
            self.set_notice(if snapshots.len() == 1 {
                "Search result changed type and is no longer available"
            } else {
                "One or more search results changed type and were skipped"
            });
        } else if missing > 0 {
            self.set_notice(if snapshots.len() == 1 {
                "Search result is no longer available"
            } else {
                "One or more search results are no longer available and were skipped"
            });
        }
        valid
    }

    pub(crate) fn revalidated_search_targets(&mut self) -> Vec<PathBuf> {
        let snapshots = self
            .search_results
            .as_ref()
            .map(|view| {
                target_paths_from_hits(&view.results, view.selected)
                    .into_iter()
                    .filter_map(|path| {
                        view.results
                            .iter()
                            .find(|hit| hit.entry.path == path)
                            .map(|hit| (path, hit.entry.kind))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.revalidated_search_entries(snapshots)
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    pub(crate) fn revalidated_search_entry(&mut self) -> Option<FileEntry> {
        let snapshot = self.focused_search_entry()?.clone();
        self.revalidated_search_entries(vec![(snapshot.path, snapshot.kind)])
            .into_iter()
            .next()
    }

    pub(crate) fn activate_search_entry(&mut self, editor: bool) -> AppMode {
        let Some(entry) = self.revalidated_search_entry() else {
            return AppMode::SearchResults;
        };
        if !editor && entry.kind == EntryKind::Directory {
            self.open_search_result(&entry.path);
            AppMode::Browser
        } else {
            self.open_external_entry(&entry, editor, ReturnDestination::SearchResults)
        }
    }

    pub(crate) fn handle_search_form_key(
        &mut self,
        mut form: SearchForm,
        key: KeyEvent,
    ) -> AppMode {
        if key.code == KeyCode::Esc {
            return form.return_to.mode();
        }
        if !form.advanced
            && form.draft.name.is_empty()
            && self.config.hotkeys.search_filesystem.matches(key)
        {
            form.advanced = true;
            form.section = SearchSection::Scope;
            form.field = 0;
            return AppMode::SearchForm(form);
        }
        if key.code == KeyCode::Enter {
            return self.submit_search(form);
        }
        if !form.advanced {
            let before = form.draft.name.clone();
            edit_cursor_input(&mut form.draft.name, &mut form.cursors.name, key);
            if form.draft.name != before {
                form.error = None;
            }
            return AppMode::SearchForm(form);
        }

        if matches!(key.code, KeyCode::Up | KeyCode::Down) {
            form.section = form.section.moved(key.code == KeyCode::Down);
            form.field = 0;
            return AppMode::SearchForm(form);
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let count = form.section.field_count();
            form.field = if key.code == KeyCode::BackTab {
                (form.field + count - 1) % count
            } else {
                (form.field + 1) % count
            };
            return AppMode::SearchForm(form);
        }
        if form.section == SearchSection::Filters
            && form.field <= 4
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            form.field = if key.code == KeyCode::Left {
                (form.field + 4) % 5
            } else {
                (form.field + 1) % 5
            };
            return AppMode::SearchForm(form);
        }
        if let Some(changed) = form.edit_active_text(key) {
            if changed {
                form.error = None;
            }
            return AppMode::SearchForm(form);
        }
        let space = key.code == KeyCode::Char(' ');
        let entry_kind_toggle = form.section == SearchSection::Filters && form.field <= 4 && space;
        if !space
            && matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            let before = form.draft.name.clone();
            edit_cursor_input(&mut form.draft.name, &mut form.cursors.name, key);
            if form.draft.name != before {
                form.error = None;
            }
            return AppMode::SearchForm(form);
        }
        if (entry_kind_toggle || matches!(key.code, KeyCode::Left | KeyCode::Right))
            && form.handle_choice_key(key)
        {
            form.error = None;
        }
        AppMode::SearchForm(form)
    }

    pub(crate) fn submit_search(&mut self, mut form: SearchForm) -> AppMode {
        if self.search.is_some() {
            return AppMode::SearchProgress;
        }
        let request = match form.draft.compile(search::ripgrep_available()) {
            Ok(request) => request,
            Err(error) => {
                form.error = Some(error.to_string());
                return AppMode::SearchForm(form);
            }
        };
        self.search_return = form.return_to;
        if self.search_results.is_some() {
            self.previous_search_results = self.search_results.take();
        } else {
            self.previous_search_results = None;
        }
        self.search_matches = 0;
        self.search_skipped = 0;
        self.search_cancelling = false;
        self.search_results = Some(SearchView {
            request,
            results: Vec::new(),
            selected: 0,
            selected_path: None,
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        let request = self
            .search_results
            .as_ref()
            .expect("search view")
            .request
            .clone();
        self.search = Some(search::spawn(request));
        AppMode::SearchProgress
    }

    pub(crate) fn handle_search_progress_key(&mut self, key: KeyEvent) -> AppMode {
        if key.code == KeyCode::Esc {
            if let Some(search) = &self.search {
                search.cancel.store(true, Ordering::Relaxed);
                self.search_cancelling = true;
            }
        }
        AppMode::SearchProgress
    }

    pub(crate) fn handle_search_results_key(&mut self, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        let return_to = ReturnDestination::SearchResults;
        if key.code == KeyCode::Esc {
            AppMode::Browser
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            if let Some(view) = &mut self.search_results {
                if !view.results.is_empty() {
                    view.selected = (view.selected + 1).min(view.results.len() - 1);
                    view.selected_path = Some(view.results[view.selected].entry.path.clone());
                }
            }
            AppMode::SearchResults
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            if let Some(view) = &mut self.search_results {
                view.selected = view.selected.saturating_sub(1);
                view.selected_path = view
                    .results
                    .get(view.selected)
                    .map(|hit| hit.entry.path.clone());
            }
            AppMode::SearchResults
        } else if key.code == KeyCode::Enter
            || key.code == KeyCode::Right
            || hotkeys.expand.matches(key)
        {
            self.activate_search_entry(false)
        } else if hotkeys.select.matches(key) {
            if let Some(view) = &mut self.search_results {
                if let Some(hit) = view.results.get_mut(view.selected) {
                    hit.entry.selected = !hit.entry.selected;
                }
            }
            AppMode::SearchResults
        } else if hotkeys.copy.matches(key) || hotkeys.cut.matches(key) {
            let mode = if hotkeys.copy.matches(key) {
                ClipboardMode::Copy
            } else {
                ClipboardMode::Cut
            };
            let expected = self.search_target_paths().len();
            let paths = self.revalidated_search_targets();
            let skipped = paths.len() != expected;
            if !paths.is_empty() {
                self.set_clipboard_paths(mode, paths);
            }
            if skipped {
                self.set_notice(
                    "One or more search results are no longer available and were skipped",
                );
            }
            AppMode::SearchResults
        } else if hotkeys.rename.matches(key) {
            let Some(entry) = self.revalidated_search_entry() else {
                return AppMode::SearchResults;
            };
            let input = entry.name.clone();
            self.modal_return = return_to;
            AppMode::Prompt(Prompt::Rename {
                source: entry.path,
                cursor: input.chars().count(),
                input,
            })
        } else if hotkeys.trash.matches(key) {
            let paths = self.revalidated_search_targets();
            if paths.is_empty() {
                return AppMode::SearchResults;
            }
            let Some(paths) = self.mutation_targets_from(paths) else {
                return AppMode::SearchResults;
            };
            self.modal_return = return_to;
            AppMode::Prompt(Prompt::ConfirmTrash { paths })
        } else if hotkeys.quick_trash.matches(key) {
            let paths = self.revalidated_search_targets();
            if paths.is_empty() {
                return AppMode::SearchResults;
            }
            let Some(paths) = self.mutation_targets_from(paths) else {
                return AppMode::SearchResults;
            };
            self.operation_return = return_to;
            self.start_trash(paths);
            AppMode::Progress
        } else if hotkeys.archive.matches(key) {
            self.archive_return = return_to;
            self.modal_return = return_to;
            let paths = self.revalidated_search_targets();
            if paths.is_empty() {
                AppMode::SearchResults
            } else {
                self.prepare_archive_paths(paths)
            }
        } else if hotkeys.open.matches(key) || hotkeys.edit.matches(key) {
            self.activate_search_entry(hotkeys.edit.matches(key))
        } else if hotkeys.info.matches(key) {
            let Some(entry) = self.revalidated_search_entry() else {
                return AppMode::SearchResults;
            };
            self.modal_return = return_to;
            AppMode::Info(Some(entry))
        } else if hotkeys.paste.matches(key)
            || hotkeys.create_file.matches(key)
            || hotkeys.create_directory.matches(key)
        {
            self.set_notice("Unavailable in search results");
            AppMode::SearchResults
        } else if hotkeys.search.matches(key) {
            AppMode::SearchForm(SearchForm::quick(
                self.current_dir.clone(),
                SearchReturn::Results,
            ))
        } else if hotkeys.search_filesystem.matches(key) {
            AppMode::SearchForm(SearchForm::advanced(
                PathBuf::from("/"),
                SearchScope::Filesystem,
                SearchReturn::Results,
            ))
        } else {
            AppMode::SearchResults
        }
    }
}
