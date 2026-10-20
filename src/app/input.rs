use super::*;

impl App {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.kind == KeyEventKind::Repeat
            && matches!(self.mode, AppMode::Browser)
            && !matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !self.config.hotkeys.up.matches(key)
            && !self.config.hotkeys.down.matches(key)
        {
            return;
        }
        // This match is the modal-isolation boundary. Browser shortcuts are never
        // considered while any prompt, popup, error, or progress mode owns focus.
        let mode = std::mem::replace(&mut self.mode, AppMode::Browser);
        self.mode = match mode {
            AppMode::Browser => self.handle_browser_key(key),
            AppMode::SearchForm(form) => self.handle_search_form_key(form, key),
            AppMode::Archive(view) => self.handle_archive_key(view, key),
            AppMode::Tools(view) => self.handle_tools_key(view, key),
            AppMode::Prompt(prompt) => self.handle_prompt_key(prompt, key),
            AppMode::Progress => self.handle_progress_key(key),
            AppMode::SearchProgress => self.handle_search_progress_key(key),
            AppMode::SearchResults => self.handle_search_results_key(key),
            AppMode::UpdateProgress => AppMode::UpdateProgress,
            AppMode::Trash(view) => self.handle_trash_key(view, key),
            AppMode::Devices(view) => self.handle_device_key(view, key),
            AppMode::Network(view) => self.handle_network_key(view, key),
            AppMode::NetworkProgress => AppMode::NetworkProgress,
            AppMode::Partitions(view) => self.handle_partition_key(view, key),
            AppMode::Help => self.handle_readonly_popup(key, AppMode::Help),
            AppMode::Info(entry) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
                    || self.config.hotkeys.quit.matches(key)
                {
                    self.modal_return.mode()
                } else {
                    AppMode::Info(entry)
                }
            }
            AppMode::ConfigError { path, error } => self.handle_config_error(path, error, key),
        };
    }

    pub(crate) fn handle_browser_key(&mut self, key: KeyEvent) -> AppMode {
        self.modal_return = ReturnDestination::Browser;
        self.operation_return = ReturnDestination::Browser;
        self.archive_return = ReturnDestination::Browser;
        let hotkeys = self.config.hotkeys.clone();
        if hotkeys.force_quit.matches(key) {
            self.running = false;
            return AppMode::Browser;
        }
        if hotkeys.quit.matches(key) {
            self.running = false;
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            self.move_cursor(1);
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            self.move_cursor(-1);
        } else if key.code == KeyCode::Enter {
            return match self.browser_view {
                BrowserView::Tree => self.activate_tree_entry(),
                BrowserView::Table => self.open_selected_table(),
            };
        } else if key.code == KeyCode::Right || hotkeys.expand.matches(key) {
            return match self.browser_view {
                BrowserView::Tree => self.tree_right(),
                BrowserView::Table => self.open_selected_table(),
            };
        } else if key.code == KeyCode::Left || hotkeys.collapse.matches(key) {
            match self.browser_view {
                BrowserView::Tree => self.tree_left(),
                BrowserView::Table => self.go_parent(),
            }
        } else if hotkeys.toggle_view.matches(key) {
            self.toggle_browser_view();
        } else if hotkeys.select.matches(key) {
            self.toggle_selection();
        } else if hotkeys.hidden.matches(key) {
            self.config.ui.show_hidden = !self.config.ui.show_hidden;
            self.refresh();
        } else if hotkeys.sort.matches(key) {
            self.cycle_sort();
            self.refresh();
        } else if hotkeys.reverse_sort.matches(key) {
            self.config.ui.reverse_sort = !self.config.ui.reverse_sort;
            self.refresh();
        } else if hotkeys.go_to.matches(key) {
            return AppMode::Prompt(Prompt::GoTo {
                input: String::new(),
            });
        } else if hotkeys.search.matches(key) {
            return AppMode::SearchForm(SearchForm::quick(
                self.current_dir.clone(),
                SearchReturn::Browser,
            ));
        } else if hotkeys.search_filesystem.matches(key) {
            return AppMode::SearchForm(SearchForm::advanced(
                PathBuf::from("/"),
                SearchScope::Filesystem,
                SearchReturn::Browser,
            ));
        } else if hotkeys.rename.matches(key) {
            if let Some(entry) = self.selected_entry() {
                let input = entry.name.clone();
                return AppMode::Prompt(Prompt::Rename {
                    source: entry.path.clone(),
                    cursor: input.chars().count(),
                    input,
                });
            }
        } else if hotkeys.create_directory.matches(key) {
            return AppMode::Prompt(Prompt::CreateDirectory {
                input: String::new(),
            });
        } else if hotkeys.create_file.matches(key) {
            return AppMode::Prompt(Prompt::CreateFile {
                input: String::new(),
                cursor: 0,
            });
        } else if hotkeys.copy.matches(key) {
            self.set_clipboard(ClipboardMode::Copy);
        } else if hotkeys.cut.matches(key) {
            self.set_clipboard(ClipboardMode::Cut);
        } else if hotkeys.paste.matches(key) {
            return self.prepare_paste();
        } else if hotkeys.archive.matches(key) {
            return self.prepare_archive();
        } else if hotkeys.trash.matches(key) {
            if let Some(paths) = self.mutation_targets() {
                return AppMode::Prompt(Prompt::ConfirmTrash { paths });
            }
        } else if hotkeys.quick_trash.matches(key) {
            if let Some(paths) = self.mutation_targets() {
                self.start_trash(paths);
                return AppMode::Progress;
            }
        } else if hotkeys.trash_bin.matches(key) {
            return self.open_trash();
        } else if hotkeys.tools.matches(key) {
            return AppMode::Tools(ToolsView { selected: 0 });
        } else if hotkeys.info.matches(key) {
            self.modal_return = ReturnDestination::Browser;
            return AppMode::Info(self.selected_entry().cloned());
        } else if hotkeys.help.matches(key) {
            return AppMode::Help;
        } else if hotkeys.open.matches(key) || hotkeys.edit.matches(key) {
            return self.open_external(hotkeys.edit.matches(key));
        } else if hotkeys.devices.matches(key) {
            return self.open_partitions(ManagerReturn::Files);
        } else if hotkeys.network_shares.matches(key) {
            return self.open_network_from(ManagerReturn::Files);
        }
        AppMode::Browser
    }

    pub(crate) fn handle_tools_key(&mut self, mut view: ToolsView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        if key.code == KeyCode::Esc || hotkeys.quit.matches(key) || hotkeys.tools.matches(key) {
            AppMode::Browser
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            view.selected = (view.selected + 1).min(BuiltinTool::ALL.len() - 1);
            AppMode::Tools(view)
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            view.selected = view.selected.saturating_sub(1);
            AppMode::Tools(view)
        } else if key.code == KeyCode::Enter
            || key.code == KeyCode::Right
            || hotkeys.expand.matches(key)
        {
            match BuiltinTool::ALL.get(view.selected).copied() {
                Some(BuiltinTool::DeviceManager) => self.open_partitions(ManagerReturn::Tools),
                Some(BuiltinTool::NetworkShares) => self.open_network_from(ManagerReturn::Tools),
                None => AppMode::Tools(view),
            }
        } else {
            AppMode::Tools(view)
        }
    }

    pub(crate) fn handle_archive_key(&mut self, mut view: ArchiveView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        if key.code == KeyCode::Esc || hotkeys.quit.matches(key) {
            self.archive_return.mode()
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            if !view.entries.is_empty() {
                view.selected = (view.selected + 1).min(view.entries.len() - 1);
            }
            AppMode::Archive(view)
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            view.selected = view.selected.saturating_sub(1);
            AppMode::Archive(view)
        } else {
            AppMode::Archive(view)
        }
    }

    pub(crate) fn handle_prompt_key(&mut self, mut prompt: Prompt, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match &mut prompt {
            Prompt::GoTo { input } => {
                if edit_input(input, key) {
                    return AppMode::Browser;
                }
                if key.code == KeyCode::Enter {
                    self.go_to(PathBuf::from(expand_home(input)));
                    return AppMode::Browser;
                }
            }
            Prompt::CreateDirectory { input } => {
                if edit_input(input, key) {
                    return AppMode::Browser;
                }
                if key.code == KeyCode::Enter {
                    self.create_directory(input);
                    return AppMode::Browser;
                }
            }
            Prompt::CreateFile { input, cursor } => {
                if edit_cursor_input(input, cursor, key) {
                    return AppMode::Browser;
                }
                if key.code == KeyCode::Enter {
                    let input = input.clone();
                    return self.create_file(&input);
                }
            }
            Prompt::ArchiveFormat { sources, selected } => {
                if key.code == KeyCode::Esc {
                    return self.archive_return.mode();
                }
                if key.code == KeyCode::Down || hotkeys.down.matches(key) {
                    *selected = (*selected + 1).min(ArchiveFormat::ALL.len() - 1);
                } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
                    *selected = selected.saturating_sub(1);
                } else if key.code == KeyCode::Enter {
                    let format = ArchiveFormat::ALL[*selected];
                    let input = suggested_archive_name(sources, format);
                    return AppMode::Prompt(Prompt::ArchiveName {
                        sources: sources.clone(),
                        format,
                        cursor: input.chars().count(),
                        input,
                    });
                }
            }
            Prompt::ArchiveName {
                sources,
                format,
                input,
                cursor,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.archive_return.mode();
                }
                if key.code == KeyCode::Enter {
                    let name = format.append_extension(input.trim());
                    if let Err(error) = validate_archive_name(&name) {
                        self.status = error;
                        return AppMode::Prompt(prompt);
                    }
                    let destination = self.current_dir.join(name);
                    self.start_archive(ArchiveRequest::Create {
                        sources: sources.clone(),
                        destination,
                        format: *format,
                    });
                    return AppMode::Progress;
                }
            }
            Prompt::ArchiveActions { archive, selected } => {
                if key.code == KeyCode::Esc {
                    return self.archive_return.mode();
                }
                if key.code == KeyCode::Down || hotkeys.down.matches(key) {
                    *selected = (*selected + 1).min(1);
                } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
                    *selected = selected.saturating_sub(1);
                } else if key.code == KeyCode::Enter {
                    if *selected == 0 {
                        self.start_archive(ArchiveRequest::List {
                            archive: archive.clone(),
                        });
                        return AppMode::Progress;
                    }
                    if self.config.behavior.read_only {
                        self.status = "Read-only mode: archive extraction is disabled".into();
                        return self.archive_return.mode();
                    }
                    let input = self.current_dir.display().to_string();
                    return AppMode::Prompt(Prompt::ArchiveDestination {
                        archive: archive.clone(),
                        cursor: input.chars().count(),
                        input,
                    });
                }
            }
            Prompt::ArchiveDestination {
                archive,
                input,
                cursor,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.archive_return.mode();
                }
                if key.code == KeyCode::Enter {
                    let destination = PathBuf::from(expand_home(input.trim()));
                    let destination = if destination.is_absolute() {
                        destination
                    } else {
                        self.current_dir.join(destination)
                    };
                    if !destination.is_dir() {
                        self.status = format!(
                            "Extraction destination is not a directory: {}",
                            destination.display()
                        );
                        return AppMode::Prompt(prompt);
                    }
                    self.start_archive(ArchiveRequest::Extract {
                        archive: archive.clone(),
                        destination,
                    });
                    return AppMode::Progress;
                }
            }
            Prompt::Rename {
                source,
                input,
                cursor,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.modal_return.mode();
                }
                if key.code == KeyCode::Enter {
                    let source = source.clone();
                    let input = input.clone();
                    self.rename(&source, &input);
                    return self.modal_return.mode();
                }
            }
            Prompt::ConfirmTrash { paths } => match key.code {
                KeyCode::Enter => {
                    self.operation_return = self.modal_return;
                    self.start_trash(paths.clone());
                    return AppMode::Progress;
                }
                _ if hotkeys.confirm_yes.matches(key) => {
                    self.operation_return = self.modal_return;
                    self.start_trash(paths.clone());
                    return AppMode::Progress;
                }
                KeyCode::Esc => return self.modal_return.mode(),
                _ if hotkeys.confirm_no.matches(key) => return self.modal_return.mode(),
                _ => {}
            },
            Prompt::ConfirmOverwrite {
                sources,
                destination,
                cut,
            } => match key.code {
                KeyCode::Enter => {
                    self.reveal_browser_directory(destination);
                    self.start_copy(sources.clone(), destination.clone(), *cut, true);
                    return AppMode::Progress;
                }
                _ if hotkeys.overwrite.matches(key) => {
                    self.reveal_browser_directory(destination);
                    self.start_copy(sources.clone(), destination.clone(), *cut, true);
                    return AppMode::Progress;
                }
                _ if hotkeys.skip.matches(key) => {
                    let filtered = sources
                        .iter()
                        .filter(|source| {
                            source
                                .file_name()
                                .map(|name| !destination.join(name).exists())
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if filtered.is_empty() {
                        self.set_notice("All conflicting items were skipped");
                        return AppMode::Browser;
                    }
                    self.reveal_browser_directory(destination);
                    self.start_copy(filtered, destination.clone(), *cut, false);
                    return AppMode::Progress;
                }
                KeyCode::Esc => return AppMode::Browser,
                _ if hotkeys.abort.matches(key) => return AppMode::Browser,
                _ => {}
            },
            Prompt::ConfirmRestore { entries, manager } => {
                if key.code == KeyCode::Enter || hotkeys.restore.matches(key) {
                    return self.restore_trash_entries(entries, manager);
                }
                if key.code == KeyCode::Esc {
                    return self.open_trash();
                }
            }
            Prompt::ConfirmPermanentDelete {
                entries, manager, ..
            } => match key.code {
                KeyCode::Enter => {
                    self.start_permanent_delete(entries.clone(), manager.clone());
                    return AppMode::Progress;
                }
                _ if hotkeys.permanent_delete.matches(key) => {
                    self.start_permanent_delete(entries.clone(), manager.clone());
                    return AppMode::Progress;
                }
                KeyCode::Esc => return self.open_trash(),
                _ => {}
            },
            Prompt::ConfirmLuks { action, .. } => match key.code {
                KeyCode::Enter => {
                    self.start_luks(action.clone(), None);
                    return AppMode::Progress;
                }
                KeyCode::Esc => {
                    if let Some(pending) = self.partition_preflight.take() {
                        return AppMode::Partitions(pending.view);
                    }
                    return self.reopen_partitions();
                }
                _ => {}
            },
            Prompt::LuksPassphrase {
                source,
                label,
                size,
                input,
                error,
            } => match key.code {
                KeyCode::Esc => return self.reopen_partitions(),
                KeyCode::Enter if !input.is_empty() => {
                    let retry = LuksRetry {
                        source: source.clone(),
                        label: label.clone(),
                        size: *size,
                    };
                    let action = LuksAction::UnlockAndMount {
                        source: source.clone(),
                        passphrase: std::mem::take(input),
                    };
                    *error = None;
                    self.start_luks(action, Some(retry));
                    return AppMode::Progress;
                }
                KeyCode::Backspace => {
                    error.take();
                    input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    error.take();
                    input.push(character);
                }
                _ => {}
            },
            Prompt::PartitionAuthentication {
                action,
                view,
                input,
                error,
            } => match key.code {
                KeyCode::Esc => return AppMode::Partitions(view.clone()),
                KeyCode::Enter if input.is_empty() => {
                    *error = Some("Enter your administrator password".into());
                }
                KeyCode::Enter => {
                    let password = std::mem::take(input);
                    self.start_partition_operation(action.clone(), view.clone(), Some(password));
                    return AppMode::Progress;
                }
                KeyCode::Backspace => {
                    error.take();
                    input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    error.take();
                    input.push(character);
                }
                _ => {}
            },
            Prompt::PartitionError { view, .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    let selected_path = view
                        .entries
                        .get(view.selected)
                        .map(|entry| entry.device.path.clone());
                    self.start_partition_refresh(selected_path);
                    return AppMode::Partitions(view.clone());
                }
            }
            Prompt::Mounted { path } => match key.code {
                KeyCode::Enter => {
                    let path = path.clone();
                    self.go_to(path);
                    return AppMode::Browser;
                }
                KeyCode::Esc => return AppMode::Browser,
                _ => {}
            },
            Prompt::SmbAddress {
                input,
                cursor,
                error,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.open_network();
                }
                if key.code == KeyCode::Enter {
                    match ShareAddress::parse(input) {
                        Ok(address) => {
                            return AppMode::Prompt(Prompt::SmbUsername {
                                address,
                                input: String::new(),
                                cursor: 0,
                                error: None,
                            })
                        }
                        Err(message) => *error = Some(message),
                    }
                } else if matches!(
                    key.code,
                    KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                ) {
                    error.take();
                }
            }
            Prompt::SmbUsername {
                address,
                input,
                cursor,
                error,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.open_network();
                }
                if key.code == KeyCode::Enter {
                    let username = input.trim().to_owned();
                    if username.is_empty() {
                        self.start_network_action(NetworkAction::Connect(ConnectRequest {
                            address: address.clone(),
                            auth: NetworkAuth::Anonymous,
                        }));
                        return AppMode::NetworkProgress;
                    }
                    if username.chars().any(char::is_control) {
                        *error = Some("The username contains invalid characters".into());
                    } else {
                        return AppMode::Prompt(Prompt::SmbDomain {
                            address: address.clone(),
                            username,
                            input: String::new(),
                            cursor: 0,
                        });
                    }
                }
            }
            Prompt::SmbDomain {
                address,
                username,
                input,
                cursor,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return self.open_network();
                }
                if key.code == KeyCode::Enter {
                    return AppMode::Prompt(Prompt::SmbPassword {
                        address: address.clone(),
                        username: username.clone(),
                        domain: input.trim().to_owned(),
                        input: NetworkSecret::default(),
                        error: None,
                    });
                }
            }
            Prompt::SmbPassword {
                address,
                username,
                domain,
                input,
                error,
            } => match key.code {
                KeyCode::Esc => return self.open_network(),
                KeyCode::Enter if !input.is_empty() => {
                    let request = ConnectRequest {
                        address: address.clone(),
                        auth: NetworkAuth::Password {
                            username: username.clone(),
                            domain: domain.clone(),
                            password: std::mem::take(input),
                            remember: false,
                        },
                    };
                    *error = None;
                    if self.network_secret_storage_available {
                        return AppMode::Prompt(Prompt::SmbRemember {
                            request,
                            available: true,
                        });
                    }
                    self.set_notice("Secret storage unavailable · using this session only");
                    self.start_network_action(NetworkAction::Connect(request));
                    return AppMode::NetworkProgress;
                }
                KeyCode::Backspace => {
                    error.take();
                    input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    error.take();
                    input.push(character);
                }
                _ => {}
            },
            Prompt::SmbRemember { request, .. } => match key.code {
                _ if hotkeys.confirm_yes.matches(key) => {
                    if let NetworkAuth::Password { remember, .. } = &mut request.auth {
                        *remember = true;
                    }
                    self.start_network_action(NetworkAction::Connect(request.clone()));
                    return AppMode::NetworkProgress;
                }
                KeyCode::Enter => {
                    self.start_network_action(NetworkAction::Connect(request.clone()));
                    return AppMode::NetworkProgress;
                }
                _ if hotkeys.confirm_no.matches(key) => {
                    self.start_network_action(NetworkAction::Connect(request.clone()));
                    return AppMode::NetworkProgress;
                }
                KeyCode::Esc => return self.open_network(),
                _ => {}
            },
            Prompt::ConfirmSmbDisconnect { share } => match key.code {
                KeyCode::Enter => {
                    self.start_network_action(NetworkAction::Disconnect(share.clone()));
                    return AppMode::NetworkProgress;
                }
                _ if hotkeys.network_disconnect.matches(key) => {
                    self.start_network_action(NetworkAction::Disconnect(share.clone()));
                    return AppMode::NetworkProgress;
                }
                KeyCode::Esc => return self.open_network(),
                _ => {}
            },
            Prompt::ConfirmSmbForget { share } => match key.code {
                KeyCode::Enter => {
                    self.start_network_action(NetworkAction::Forget(share.clone()));
                    return AppMode::NetworkProgress;
                }
                _ if hotkeys.network_forget.matches(key) => {
                    self.start_network_action(NetworkAction::Forget(share.clone()));
                    return AppMode::NetworkProgress;
                }
                KeyCode::Esc => return self.open_network(),
                _ => {}
            },
            Prompt::SmbMounted { path, .. } => match key.code {
                KeyCode::Enter => {
                    let path = path.clone();
                    self.go_to(path);
                    return AppMode::Browser;
                }
                KeyCode::Esc => return self.open_network(),
                _ => {}
            },
            Prompt::SmbMessage {
                return_to_network, ..
            } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return if *return_to_network {
                        self.open_network()
                    } else {
                        AppMode::Browser
                    };
                }
            }
            Prompt::UpdateAvailable { latest, .. } => match key.code {
                KeyCode::Enter => {
                    let latest = latest.clone();
                    return if self.start_update(&latest) {
                        AppMode::UpdateProgress
                    } else {
                        AppMode::Prompt(Prompt::Message {
                            title: "Update failed".into(),
                            body: "Could not locate the installed minfm binary".into(),
                        })
                    };
                }
                KeyCode::Esc => return AppMode::Browser,
                _ => {}
            },
            Prompt::Message { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return self.modal_return.mode();
                }
            }
            Prompt::SmartReport { body, scroll, view } => match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    let selected_path = view
                        .entries
                        .get(view.selected)
                        .map(|entry| entry.device.path.clone());
                    let view = view.clone();
                    self.start_partition_refresh(selected_path);
                    return AppMode::Partitions(view);
                }
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => {
                    *scroll = scroll
                        .saturating_add(1)
                        .min(smart_report_scroll_limit(body));
                }
                KeyCode::PageUp => *scroll = scroll.saturating_sub(8),
                KeyCode::PageDown => {
                    *scroll = scroll
                        .saturating_add(8)
                        .min(smart_report_scroll_limit(body));
                }
                _ => {}
            },
            Prompt::OpenError { config_error, .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return if let Some((path, error)) = config_error.take() {
                        AppMode::ConfigError { path, error }
                    } else {
                        self.modal_return.mode()
                    };
                }
            }
            Prompt::Summary {
                return_to_trash, ..
            } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return if let Some(manager) = return_to_trash.clone() {
                        self.open_trash_manager(manager)
                    } else {
                        self.operation_return.mode()
                    };
                }
            }
        }
        AppMode::Prompt(prompt)
    }

    pub(crate) fn handle_progress_key(&mut self, key: KeyEvent) -> AppMode {
        if key.code == KeyCode::Esc && self.progress.cancellable {
            if let Some(operation) = &self.operation {
                operation.cancel.store(true, Ordering::Relaxed);
                self.progress.cancelling = true;
            }
            if let Some(operation) = &self.archive_operation {
                operation.cancel.store(true, Ordering::Relaxed);
                self.progress.cancelling = true;
            }
        }
        AppMode::Progress
    }

    pub(crate) fn handle_trash_key(&mut self, mut view: TrashView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        if key.code == KeyCode::Esc || hotkeys.trash_bin.matches(key) {
            self.refresh();
            AppMode::Browser
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            if !view.entries.is_empty() {
                view.selected = (view.selected + 1).min(view.entries.len() - 1);
            }
            AppMode::Trash(view)
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            view.selected = view.selected.saturating_sub(1);
            AppMode::Trash(view)
        } else if hotkeys.select.matches(key) {
            if let Some(entry) = view.entries.get(view.selected) {
                if !view.marked.remove(&entry.trashed_path) {
                    view.marked.insert(entry.trashed_path.clone());
                }
            }
            AppMode::Trash(view)
        } else if key.code == KeyCode::Enter || hotkeys.restore.matches(key) {
            let entries = trash_targets(&view);
            if entries.is_empty() {
                AppMode::Trash(view)
            } else {
                AppMode::Prompt(Prompt::ConfirmRestore {
                    entries,
                    manager: view.manager,
                })
            }
        } else if hotkeys.permanent_delete.matches(key) {
            let entries = trash_targets(&view);
            if entries.is_empty() {
                AppMode::Trash(view)
            } else {
                let total_bytes = entries.iter().map(TrashEntry::estimated_size).sum();
                AppMode::Prompt(Prompt::ConfirmPermanentDelete {
                    entries,
                    manager: view.manager,
                    clear_all: false,
                    total_bytes,
                })
            }
        } else if hotkeys.quick_permanent_delete.matches(key) {
            let entries = trash_targets(&view);
            if entries.is_empty() {
                AppMode::Trash(view)
            } else {
                self.start_permanent_delete(entries, view.manager);
                AppMode::Progress
            }
        } else if hotkeys.clear_trash.matches(key) {
            if view.entries.is_empty() {
                AppMode::Trash(view)
            } else {
                let total_bytes = view.entries.iter().map(TrashEntry::estimated_size).sum();
                AppMode::Prompt(Prompt::ConfirmPermanentDelete {
                    entries: view.entries.clone(),
                    manager: view.manager,
                    clear_all: true,
                    total_bytes,
                })
            }
        } else {
            AppMode::Trash(view)
        }
    }

    pub(crate) fn handle_readonly_popup(&mut self, key: KeyEvent, mode: AppMode) -> AppMode {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
            || self.config.hotkeys.quit.matches(key)
        {
            AppMode::Browser
        } else {
            mode
        }
    }

    pub(crate) fn handle_config_error(
        &mut self,
        path: PathBuf,
        error: String,
        key: KeyEvent,
    ) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match key.code {
            KeyCode::Esc => {
                self.running = false;
                AppMode::ConfigError { path, error }
            }
            _ if hotkeys.quit.matches(key) => {
                self.running = false;
                AppMode::ConfigError { path, error }
            }
            _ if hotkeys.config_reload.matches(key) => match config::load_from(path.clone()) {
                ConfigLoad::Valid { config, path } => {
                    self.config = config;
                    self.config_path = path;
                    self.refresh();
                    self.set_notice("Configuration reloaded");
                    AppMode::Browser
                }
                ConfigLoad::Invalid { path, error } => AppMode::ConfigError { path, error },
            },
            _ if hotkeys.config_edit.matches(key) => {
                let program = launcher::resolve_editor(&self.config.open.editor);
                if launcher::is_terminal_editor(&program) {
                    self.pending_terminal_editor = Some(PendingTerminalEditor {
                        program,
                        path: path.clone(),
                        browser: None,
                        return_to: ReturnDestination::Browser,
                    });
                    return AppMode::ConfigError { path, error };
                }
                match launcher::launch(program, path.clone(), self.launch_sender.clone(), |error| {
                    PendingLaunchError {
                        error,
                        return_to: ReturnDestination::Browser,
                    }
                }) {
                    Ok(()) => AppMode::ConfigError { path, error },
                    Err(launch_error) => AppMode::Prompt(Prompt::OpenError {
                        body: launch_error.to_string(),
                        config_error: Some((path, error)),
                    }),
                }
            }
            _ => AppMode::ConfigError { path, error },
        }
    }
}
