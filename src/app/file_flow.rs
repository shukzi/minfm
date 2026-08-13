use super::*;

impl App {
    pub(crate) fn prepare_archive(&mut self) -> AppMode {
        self.prepare_archive_paths(self.selected_paths())
    }

    pub(crate) fn prepare_archive_paths(&mut self, paths: Vec<PathBuf>) -> AppMode {
        if paths.len() == 1 && paths[0].is_file() && ArchiveFormat::detect(&paths[0]).is_some() {
            return AppMode::Prompt(Prompt::ArchiveActions {
                archive: paths[0].clone(),
                selected: 0,
            });
        }
        let Some(sources) = self.mutation_targets_from(paths) else {
            return self.modal_return.mode();
        };
        AppMode::Prompt(Prompt::ArchiveFormat {
            sources,
            selected: 0,
        })
    }

    pub(crate) fn start_archive(&mut self, request: ArchiveRequest) {
        self.progress = ProgressState {
            cancellable: true,
            ..ProgressState::default()
        };
        self.archive_operation = Some(archive::spawn(request));
    }

    pub(crate) fn set_clipboard(&mut self, mode: ClipboardMode) {
        self.set_clipboard_paths(mode, self.selected_paths());
    }

    pub(crate) fn set_clipboard_paths(&mut self, mode: ClipboardMode, paths: Vec<PathBuf>) {
        let Some(paths) = self.mutation_targets_from(paths) else {
            return;
        };
        let count = paths.len();
        self.clipboard = Some(Clipboard { mode, paths });
        self.set_notice(format!("{count} item(s) placed in file clipboard"));
    }

    pub(crate) fn prepare_paste(&mut self) -> AppMode {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: file operations are disabled".into();
            return AppMode::Browser;
        }
        let Some(clipboard) = &self.clipboard else {
            self.status = "File clipboard is empty".into();
            return AppMode::Browser;
        };
        let conflicts = clipboard.paths.iter().any(|source| {
            source
                .file_name()
                .map(|name| self.current_dir.join(name).exists())
                .unwrap_or(false)
        });
        if conflicts {
            AppMode::Prompt(Prompt::ConfirmOverwrite {
                sources: clipboard.paths.clone(),
                cut: matches!(clipboard.mode, ClipboardMode::Cut),
            })
        } else {
            self.start_copy(
                clipboard.paths.clone(),
                matches!(clipboard.mode, ClipboardMode::Cut),
                false,
            );
            AppMode::Progress
        }
    }

    pub(crate) fn start_copy(&mut self, sources: Vec<PathBuf>, cut: bool, overwrite: bool) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = None;
        self.operation_search_paths = if cut {
            self.search_results
                .as_ref()
                .map(|view| {
                    sources
                        .iter()
                        .filter(|source| view.results.iter().any(|hit| &hit.entry.path == *source))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.operation = Some(operation::spawn(OperationRequest::Copy {
            sources,
            destination: self.current_dir.clone(),
            cut,
            overwrite,
            verify: self.config.behavior.verify_copies,
            current_dir: self.current_dir.clone(),
            config_dir: self
                .config_path
                .parent()
                .unwrap_or(Path::new("/"))
                .to_path_buf(),
        }));
        if cut {
            self.clipboard = None;
        }
    }

    pub(crate) fn start_trash(&mut self, paths: Vec<PathBuf>) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = None;
        self.operation_search_paths = self
            .search_results
            .as_ref()
            .map(|view| {
                paths
                    .iter()
                    .filter(|path| view.results.iter().any(|hit| &hit.entry.path == *path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.operation = Some(operation::spawn(OperationRequest::Trash {
            paths,
            current_dir: self.current_dir.clone(),
            config_dir: self
                .config_path
                .parent()
                .unwrap_or(Path::new("/"))
                .to_path_buf(),
        }));
    }

    pub(crate) fn start_permanent_delete(
        &mut self,
        entries: Vec<TrashEntry>,
        manager: TrashManager,
    ) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = Some(manager.clone());
        self.operation = Some(operation::spawn(OperationRequest::PermanentlyDelete {
            entries,
            manager,
        }));
    }

    pub(crate) fn create_directory(&mut self, name: &str) {
        if self.config.behavior.read_only || name.trim().is_empty() {
            self.status = "Directory was not created".into();
            return;
        }
        let path = self.current_dir.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => self.set_notice(format!("Created {}", path.display())),
            Err(error) => self.status = format!("Could not create {}: {error}", path.display()),
        }
        self.refresh();
    }

    pub(crate) fn create_file(&mut self, name: &str) -> AppMode {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: file creation is disabled".into();
            return AppMode::Browser;
        }
        if name.trim().is_empty() || name == "." || name == ".." || name.contains('/') {
            return AppMode::Prompt(Prompt::Message {
                title: "Invalid file name".into(),
                body: "Enter a single non-empty file name.".into(),
            });
        }
        let path = self.current_dir.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                self.refresh_browser(Some(path.clone()));
                self.set_notice(format!("Created {}", path.display()));
                AppMode::Browser
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                AppMode::Prompt(Prompt::Message {
                    title: "File already exists".into(),
                    body: format!("Nothing was changed.\n\n{}", path.display()),
                })
            }
            Err(error) => AppMode::Prompt(Prompt::Message {
                title: "Could not create file".into(),
                body: format!("{}\n\n{error}", path.display()),
            }),
        }
    }

    pub(crate) fn rename(&mut self, source: &Path, name: &str) {
        if self.config.behavior.read_only || name.trim().is_empty() {
            self.status = "Rename cancelled".into();
            return;
        }
        let destination = source.parent().unwrap_or(&self.current_dir).join(name);
        if destination.exists() {
            self.status = format!("Destination exists: {}", destination.display());
            return;
        }
        let renamed = match std::fs::rename(source, &destination) {
            Ok(()) => {
                self.set_notice(format!("Renamed to {}", destination.display()));
                true
            }
            Err(error) => {
                self.status = format!("Rename failed: {error}");
                false
            }
        };
        if self.modal_return == ReturnDestination::SearchResults {
            if renamed {
                self.refresh_search_results(Some((source, &destination)));
            } else {
                self.refresh_search_results(None);
            }
        } else {
            self.refresh_browser(renamed.then_some(destination));
        }
    }

    pub(crate) fn open_trash(&mut self) -> AppMode {
        match TrashManager::for_path(&self.current_dir) {
            Ok(manager) => self.open_trash_manager(manager),
            Err(error) => AppMode::Prompt(Prompt::Message {
                title: "Trash unavailable".into(),
                body: error.to_string(),
            }),
        }
    }

    pub(crate) fn open_trash_manager(&mut self, manager: TrashManager) -> AppMode {
        match manager.list() {
            Ok(entries) => AppMode::Trash(TrashView {
                manager,
                entries,
                selected: 0,
                marked: HashSet::new(),
            }),
            Err(error) => AppMode::Prompt(Prompt::Message {
                title: "Trash unavailable".into(),
                body: error.to_string(),
            }),
        }
    }

    pub(crate) fn restore_trash_entries(
        &mut self,
        entries: &[TrashEntry],
        manager: &TrashManager,
    ) -> AppMode {
        let mut restored = 0;
        let mut failures = Vec::new();
        for entry in entries {
            match manager.restore(entry, None) {
                Ok(_) => restored += 1,
                Err(error) => failures.push((entry.trashed_path.clone(), error.to_string())),
            }
        }
        if failures.is_empty() {
            self.set_notice(format!("Restored {restored} item(s)"));
            self.refresh();
            return self.open_trash_manager(manager.clone());
        }
        AppMode::Prompt(Prompt::Summary {
            summary: OperationSummary {
                label: "Restoring".into(),
                completed: restored,
                failed: failures,
                ..OperationSummary::default()
            },
            return_to_trash: Some(manager.clone()),
        })
    }

    pub(crate) fn open_external(&mut self, editor: bool) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        self.open_external_entry(&entry, editor, ReturnDestination::Browser)
    }

    pub(crate) fn open_external_entry(
        &mut self,
        entry: &FileEntry,
        editor: bool,
        return_to: ReturnDestination,
    ) -> AppMode {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: external opening is disabled".into();
            return return_to.mode();
        };
        if editor && !entry.is_text_file() {
            return return_to.mode();
        }
        let path = entry.path.clone();
        let program = if editor {
            launcher::resolve_editor(&self.config.open.editor)
        } else {
            self.config.open.opener.clone()
        };
        if editor && launcher::is_terminal_editor(&program) {
            let selected_paths = target_paths_from(&self.entries, self.cursor)
                .into_iter()
                .collect();
            self.pending_terminal_editor = Some(PendingTerminalEditor {
                program,
                path: path.clone(),
                browser: (return_to == ReturnDestination::Browser).then(|| BrowserSnapshot {
                    cursor_path: path,
                    selected_paths,
                }),
                return_to,
            });
            return return_to.mode();
        }
        if let Err(error) =
            launcher::launch(program, path, self.launch_sender.clone(), move |error| {
                PendingLaunchError { error, return_to }
            })
        {
            self.modal_return = return_to;
            return AppMode::Prompt(Prompt::OpenError {
                body: error.to_string(),
                config_error: None,
            });
        }
        return_to.mode()
    }
}
