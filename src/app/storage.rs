use super::*;

impl App {
    pub(crate) fn handle_device_key(&mut self, mut view: DeviceView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match key.code {
            KeyCode::Esc => AppMode::Browser,
            _ if hotkeys.quit.matches(key) => AppMode::Browser,
            KeyCode::Down => {
                if !view.devices.is_empty() {
                    view.selected = (view.selected + 1).min(view.devices.len() - 1);
                }
                AppMode::Devices(view)
            }
            _ if hotkeys.down.matches(key) => {
                if !view.devices.is_empty() {
                    view.selected = (view.selected + 1).min(view.devices.len() - 1);
                }
                AppMode::Devices(view)
            }
            KeyCode::Up => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Devices(view)
            }
            _ if hotkeys.up.matches(key) => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Devices(view)
            }
            _ if hotkeys.refresh.matches(key) => {
                let selected_source = view
                    .devices
                    .get(view.selected)
                    .map(|device| device.source.clone());
                self.start_device_refresh(selected_source);
                AppMode::Devices(view)
            }
            _ if hotkeys.device_eject.matches(key) => {
                let Some(device) = view.devices.get(view.selected) else {
                    return AppMode::Devices(view);
                };
                if device.system_protected || !device.ejectable || device.eject_blocked {
                    return AppMode::Devices(view);
                }
                let steps = if !device.encrypted && device.is_mounted() {
                    "The filesystem will be unmounted and the drive safely ejected."
                } else if !device.encrypted {
                    "The drive will be safely ejected."
                } else if device.is_mounted() {
                    "The volume will be unmounted, locked, and safely ejected."
                } else if !device.is_locked() {
                    "The volume will be locked and safely ejected."
                } else {
                    "The device will be safely ejected."
                };
                AppMode::Prompt(Prompt::ConfirmLuks {
                    action: LuksAction::Eject {
                        source: device.source.clone(),
                        drive: device.drive.clone(),
                    },
                    title: "Eject removable device".into(),
                    body: format!(
                        "Device: {}\nPhysical drive: {}\n\n{}",
                        device.source.display(),
                        device.drive.display(),
                        steps,
                    ),
                })
            }
            KeyCode::Enter => self.device_action(view),
            _ if hotkeys.device_action.matches(key) || hotkeys.device_unmount.matches(key) => {
                self.device_action(view)
            }
            _ => AppMode::Devices(view),
        }
    }

    pub(crate) fn device_action(&mut self, view: DeviceView) -> AppMode {
        let Some(device) = view.devices.get(view.selected) else {
            return AppMode::Devices(view);
        };
        if device.system_protected {
            self.status = format!(
                "{} is a protected system device; disk actions are disabled",
                device.source.display()
            );
            return AppMode::Devices(view);
        }
        if !device.encrypted {
            if device.filesystem.is_none() {
                self.status = format!(
                    "{} has no directly mountable filesystem",
                    device.source.display()
                );
                return AppMode::Devices(view);
            }
            let (action, title, body) = if device.is_mounted() {
                (
                    LuksAction::UnmountFilesystem {
                        source: device.source.clone(),
                    },
                    "Unmount filesystem",
                    format!(
                        "Device: {}\nMounted at: {}\n\nOpen files and programs using this filesystem must be closed. The unmount is cancelled if the device is busy.",
                        device.source.display(),
                        device.mountpoints.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
                    ),
                )
            } else {
                (
                    LuksAction::MountFilesystem {
                        source: device.source.clone(),
                    },
                    "Mount filesystem",
                    format!("Device: {}", device.source.display()),
                )
            };
            return AppMode::Prompt(Prompt::ConfirmLuks {
                action,
                title: title.into(),
                body,
            });
        }
        if device.is_locked() {
            AppMode::Prompt(Prompt::LuksPassphrase {
                source: device.source.clone(),
                label: device.label.clone(),
                size: device.size,
                input: SecretInput::default(),
                error: None,
            })
        } else if device.is_mounted() {
            let Some(mapping) = device.mapping.clone() else {
                self.status = format!(
                    "{} changed state; refresh the device list and try again",
                    device.source.display()
                );
                return AppMode::Devices(view);
            };
            AppMode::Prompt(Prompt::ConfirmLuks {
                        action: LuksAction::UnmountAndLock {
                            source: device.source.clone(),
                            mapping,
                        },
                        title: "Unmount and lock LUKS volume".into(),
                        body: format!(
                            "Device: {}\nMapping: {}\nMounted at: {}\n\nThe volume will only be locked if unmounting succeeds.",
                            device.source.display(),
                            device.mapping.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "—".into()),
                            device.mountpoints.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "),
                        ),
                    })
        } else {
            let Some(mapping) = device.mapping.clone() else {
                self.status = format!(
                    "{} changed state; refresh the device list and try again",
                    device.source.display()
                );
                return AppMode::Devices(view);
            };
            AppMode::Prompt(Prompt::ConfirmLuks {
                action: LuksAction::Mount { mapping },
                title: "Mount unlocked LUKS volume".into(),
                body: format!(
                    "Device: {}\nMapping: {}",
                    device.source.display(),
                    device
                        .mapping
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
            })
        }
    }

    pub(crate) fn handle_partition_key(
        &mut self,
        mut view: PartitionView,
        key: KeyEvent,
    ) -> AppMode {
        if let Some(overlay) = view.overlay.take() {
            return self.handle_partition_overlay(view, overlay, key);
        }
        let hotkeys = self.config.hotkeys.clone();
        if key.code == KeyCode::Esc {
            if self.partition_return_to_apps {
                AppMode::Apps(AppsView { selected: 0 })
            } else {
                AppMode::Browser
            }
        } else if hotkeys.tools.matches(key) {
            AppMode::Apps(AppsView { selected: 0 })
        } else if hotkeys.quit.matches(key) {
            AppMode::Browser
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            if !view.entries.is_empty() {
                view.selected = (view.selected + 1).min(view.entries.len() - 1);
            }
            AppMode::Partitions(view)
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            view.selected = view.selected.saturating_sub(1);
            AppMode::Partitions(view)
        } else if hotkeys.refresh.matches(key) {
            let selected_path = view
                .entries
                .get(view.selected)
                .map(|entry| entry.device.path.clone());
            self.start_partition_refresh(selected_path);
            AppMode::Partitions(view)
        } else if key.code == KeyCode::Enter || hotkeys.partition_actions.matches(key) {
            if view.entries.is_empty() {
                AppMode::Partitions(view)
            } else {
                view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                AppMode::Partitions(view)
            }
        } else {
            AppMode::Partitions(view)
        }
    }

    pub(crate) fn handle_partition_overlay(
        &mut self,
        mut view: PartitionView,
        overlay: PartitionOverlay,
        mut key: KeyEvent,
    ) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        let list_overlay = matches!(
            &overlay,
            PartitionOverlay::Actions { .. }
                | PartitionOverlay::FormatOptions { .. }
                | PartitionOverlay::EncryptionFilesystem { .. }
                | PartitionOverlay::DiskLayoutOptions { .. }
                | PartitionOverlay::FreeRegionOptions { .. }
        );
        let confirmation_overlay = matches!(&overlay, PartitionOverlay::Confirm { .. });
        if list_overlay && hotkeys.down.matches(key) {
            key.code = KeyCode::Down;
            key.modifiers = KeyModifiers::NONE;
        } else if list_overlay && hotkeys.up.matches(key) {
            key.code = KeyCode::Up;
            key.modifiers = KeyModifiers::NONE;
        } else if (list_overlay || confirmation_overlay) && hotkeys.expand.matches(key) {
            key.code = KeyCode::Right;
            key.modifiers = KeyModifiers::NONE;
        } else if (list_overlay || confirmation_overlay) && hotkeys.collapse.matches(key) {
            key.code = KeyCode::Left;
            key.modifiers = KeyModifiers::NONE;
        } else if matches!(&overlay, PartitionOverlay::Actions { .. })
            && hotkeys.partition_actions.matches(key)
        {
            key.code = KeyCode::Esc;
            key.modifiers = KeyModifiers::NONE;
        }
        match overlay {
            PartitionOverlay::Actions { mut selected } => match key.code {
                KeyCode::Esc => AppMode::Partitions(view),
                KeyCode::Down => {
                    let tasks = self.partition_tasks_for_view(&view);
                    selected = (selected + 1).min(tasks.len().saturating_sub(1));
                    view.overlay = Some(PartitionOverlay::Actions { selected });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    view.overlay = Some(PartitionOverlay::Actions { selected });
                    AppMode::Partitions(view)
                }
                KeyCode::Enter | KeyCode::Right => {
                    let tasks = self.partition_tasks_for_view(&view);
                    let Some(task) = tasks.get(selected).copied() else {
                        view.overlay = Some(PartitionOverlay::Actions { selected });
                        return AppMode::Partitions(view);
                    };
                    if let Some(mode) = self.partition_unmount_preflight(&view, task) {
                        return mode;
                    }
                    if let Some(reason) = self.partition_task_unavailable(&view, task) {
                        self.set_notice(reason);
                        view.overlay = Some(PartitionOverlay::Actions { selected });
                        return AppMode::Partitions(view);
                    }
                    self.begin_partition_task(view, task)
                }
                _ => {
                    view.overlay = Some(PartitionOverlay::Actions { selected });
                    AppMode::Partitions(view)
                }
            },
            PartitionOverlay::FormatOptions {
                mut selected,
                mut encrypted,
            } => match key.code {
                KeyCode::Esc => {
                    let tasks = self.partition_tasks_for_view(&view);
                    view.overlay = Some(PartitionOverlay::Actions {
                        selected: tasks
                            .iter()
                            .position(|task| *task == PartitionTask::Format)
                            .unwrap_or(0),
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(Filesystem::ALL.len() - 1);
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Char('e') => {
                    if Filesystem::ALL.get(selected) != Some(&Filesystem::None) {
                        encrypted = !encrypted;
                    }
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Enter | KeyCode::Right => {
                    let Some(filesystem) = Filesystem::ALL.get(selected).copied() else {
                        view.overlay = Some(PartitionOverlay::FormatOptions {
                            selected,
                            encrypted,
                        });
                        return AppMode::Partitions(view);
                    };
                    view.overlay = Some(PartitionOverlay::FormatLabel {
                        filesystem,
                        encrypted,
                        input: String::new(),
                        cursor: 0,
                        error: None,
                    });
                    AppMode::Partitions(view)
                }
                _ => {
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                    });
                    AppMode::Partitions(view)
                }
            },
            PartitionOverlay::EncryptionFilesystem {
                mut selected,
                whole_disk,
            } => match key.code {
                KeyCode::Esc => {
                    view.overlay = Some(if whole_disk {
                        PartitionOverlay::DiskLayoutOptions {
                            selected: 2,
                            overwrite: false,
                        }
                    } else {
                        PartitionOverlay::FormatOptions {
                            selected: Filesystem::ALL.len(),
                            encrypted: false,
                        }
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(Filesystem::ALL.len() - 1);
                    view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                        selected,
                        whole_disk,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                        selected,
                        whole_disk,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Enter | KeyCode::Right => {
                    let Some(filesystem) = Filesystem::ALL.get(selected).copied() else {
                        return AppMode::Partitions(view);
                    };
                    view.overlay = Some(PartitionOverlay::EncryptionPassphrase {
                        filesystem,
                        whole_disk,
                        label: None,
                        passphrase: SecretInput::default(),
                        confirmation: SecretInput::default(),
                        confirming: false,
                        error: None,
                    });
                    AppMode::Partitions(view)
                }
                _ => {
                    view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                        selected,
                        whole_disk,
                    });
                    AppMode::Partitions(view)
                }
            },
            PartitionOverlay::EncryptionPassphrase {
                filesystem,
                whole_disk,
                label,
                mut passphrase,
                mut confirmation,
                mut confirming,
                error: _,
            } => {
                let mut error = None;
                match key.code {
                    KeyCode::Esc => {
                        view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                            selected: Filesystem::ALL
                                .iter()
                                .position(|candidate| *candidate == filesystem)
                                .unwrap_or(0),
                            whole_disk,
                        });
                        return AppMode::Partitions(view);
                    }
                    KeyCode::Enter if !confirming => {
                        if passphrase.character_count() < 8 {
                            error = Some("Use at least 8 characters".into());
                        } else {
                            confirming = true;
                        }
                    }
                    KeyCode::Enter => {
                        if passphrase != confirmation {
                            confirmation = SecretInput::default();
                            error = Some(
                                "Passphrases do not match; enter the confirmation again".into(),
                            );
                        } else {
                            let action = self.partition_encryption_action(
                                &view,
                                filesystem,
                                whole_disk,
                                label.clone(),
                                std::mem::take(&mut passphrase),
                            );
                            match action {
                                Ok(action) => {
                                    match partition::validate_snapshot(&action, &view.entries) {
                                        Ok(()) => {
                                            view.overlay = Some(PartitionOverlay::Confirm {
                                                action,
                                                yes_selected: false,
                                            });
                                            return AppMode::Partitions(view);
                                        }
                                        Err(validation) => error = Some(validation.to_string()),
                                    }
                                }
                                Err(message) => error = Some(message),
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if confirming {
                            confirmation.pop();
                        } else {
                            passphrase.pop();
                        }
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if confirming {
                            confirmation.push(character);
                        } else {
                            passphrase.push(character);
                        }
                    }
                    _ => {}
                }
                view.overlay = Some(PartitionOverlay::EncryptionPassphrase {
                    filesystem,
                    whole_disk,
                    label,
                    passphrase,
                    confirmation,
                    confirming,
                    error,
                });
                AppMode::Partitions(view)
            }
            PartitionOverlay::ChangePassphrase {
                mut old,
                mut new,
                mut confirmation,
                mut stage,
                error: _,
            } => {
                let mut error = None;
                match key.code {
                    KeyCode::Esc => {
                        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                        return AppMode::Partitions(view);
                    }
                    KeyCode::Enter if stage == 0 => {
                        if old.is_empty() {
                            error = Some("Enter the current passphrase".into());
                        } else {
                            stage = 1;
                        }
                    }
                    KeyCode::Enter if stage == 1 => {
                        if new.character_count() < 8 {
                            error = Some("Use at least 8 characters".into());
                        } else {
                            stage = 2;
                        }
                    }
                    KeyCode::Enter => {
                        if new != confirmation {
                            confirmation = SecretInput::default();
                            error = Some("Passphrases do not match".into());
                        } else if let Some(entry) = view.entries.get(view.selected) {
                            match DeviceIdentity::from_entry(entry) {
                                Ok(target) => {
                                    let action = PartitionAction::ChangeLuksPassphrase {
                                        target,
                                        old: std::mem::take(&mut old),
                                        new: std::mem::take(&mut new),
                                    };
                                    match partition::validate_snapshot(&action, &view.entries) {
                                        Ok(()) => {
                                            view.overlay = Some(PartitionOverlay::Confirm {
                                                action,
                                                yes_selected: false,
                                            });
                                            return AppMode::Partitions(view);
                                        }
                                        Err(validation) => error = Some(validation.to_string()),
                                    }
                                }
                                Err(identity) => error = Some(identity.to_string()),
                            }
                        }
                    }
                    KeyCode::Backspace => match stage {
                        0 => old.pop(),
                        1 => new.pop(),
                        _ => confirmation.pop(),
                    },
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match stage {
                            0 => old.push(character),
                            1 => new.push(character),
                            _ => confirmation.push(character),
                        }
                    }
                    _ => {}
                }
                view.overlay = Some(PartitionOverlay::ChangePassphrase {
                    old,
                    new,
                    confirmation,
                    stage,
                    error,
                });
                AppMode::Partitions(view)
            }
            PartitionOverlay::DiskLayoutOptions {
                mut selected,
                mut overwrite,
            } => match key.code {
                KeyCode::Esc => {
                    let tasks = self.partition_tasks_for_view(&view);
                    view.overlay = Some(PartitionOverlay::Actions {
                        selected: tasks
                            .iter()
                            .position(|task| *task == PartitionTask::CreateTable)
                            .unwrap_or(0),
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(2);
                    view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                        selected,
                        overwrite,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                        selected,
                        overwrite,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Enter | KeyCode::Right => {
                    match self.partition_disk_layout_action(&view, selected, overwrite) {
                        Ok(action) => {
                            if let Err(validation) =
                                partition::validate_snapshot(&action, &view.entries)
                            {
                                self.set_notice(validation.to_string());
                                view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                                    selected,
                                    overwrite,
                                });
                            } else {
                                view.overlay = Some(PartitionOverlay::Confirm {
                                    action,
                                    yes_selected: false,
                                });
                            }
                        }
                        Err(message) => {
                            self.set_notice(message);
                            view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                                selected,
                                overwrite,
                            });
                        }
                    }
                    AppMode::Partitions(view)
                }
                KeyCode::Char('w') => {
                    overwrite = !overwrite;
                    view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                        selected,
                        overwrite,
                    });
                    AppMode::Partitions(view)
                }
                _ => {
                    view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                        selected,
                        overwrite,
                    });
                    AppMode::Partitions(view)
                }
            },
            PartitionOverlay::FreeRegionOptions { mut selected } => {
                let regions = view
                    .entries
                    .get(view.selected)
                    .map(|disk| partition::free_regions(disk, &view.entries))
                    .unwrap_or_default();
                match key.code {
                    KeyCode::Esc => {
                        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                    }
                    KeyCode::Down => {
                        selected = (selected + 1).min(regions.len().saturating_sub(1));
                        view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected });
                    }
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                        view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected });
                    }
                    KeyCode::Enter | KeyCode::Right => {
                        if let Some((start_bytes, maximum_end)) = regions.get(selected).copied() {
                            view.overlay = Some(PartitionOverlay::PartitionSize {
                                start_bytes,
                                maximum_end,
                                input: "max".into(),
                                cursor: 3,
                                error: None,
                            });
                        } else {
                            view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected });
                        }
                    }
                    _ => {
                        view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected });
                    }
                }
                AppMode::Partitions(view)
            }
            PartitionOverlay::PartitionSize {
                start_bytes,
                maximum_end,
                mut input,
                mut cursor,
                error: _,
            } => {
                if key.code == KeyCode::Esc {
                    view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected: 0 });
                    return AppMode::Partitions(view);
                }
                let mut error = None;
                if key.code == KeyCode::Enter {
                    match self.partition_create_action_for_region(
                        &view,
                        start_bytes,
                        maximum_end,
                        &input,
                    ) {
                        Ok(action) => match partition::validate_snapshot(&action, &view.entries) {
                            Ok(()) => {
                                view.overlay = Some(PartitionOverlay::Confirm {
                                    action,
                                    yes_selected: false,
                                });
                                return AppMode::Partitions(view);
                            }
                            Err(validation) => error = Some(validation.to_string()),
                        },
                        Err(message) => error = Some(message),
                    }
                } else {
                    let _ = edit_cursor_input(&mut input, &mut cursor, key);
                }
                view.overlay = Some(PartitionOverlay::PartitionSize {
                    start_bytes,
                    maximum_end,
                    input,
                    cursor,
                    error,
                });
                AppMode::Partitions(view)
            }
            PartitionOverlay::FormatLabel {
                filesystem,
                encrypted,
                mut input,
                mut cursor,
                error: _,
            } => {
                if key.code == KeyCode::Esc {
                    let selected = Filesystem::ALL
                        .iter()
                        .position(|candidate| *candidate == filesystem)
                        .unwrap_or(0);
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                    });
                    return AppMode::Partitions(view);
                }
                let mut error = None;
                if key.code == KeyCode::Enter {
                    if encrypted {
                        view.overlay = Some(PartitionOverlay::EncryptionPassphrase {
                            filesystem,
                            whole_disk: false,
                            label: (!input.trim().is_empty()).then(|| input.trim().to_owned()),
                            passphrase: SecretInput::default(),
                            confirmation: SecretInput::default(),
                            confirming: false,
                            error: None,
                        });
                        return AppMode::Partitions(view);
                    }
                    match self.partition_format_action(&view, filesystem, &input) {
                        Ok(action) => {
                            if let Err(validation) =
                                partition::validate_snapshot(&action, &view.entries)
                            {
                                error = Some(validation.to_string());
                            } else {
                                view.overlay = Some(PartitionOverlay::Confirm {
                                    action,
                                    yes_selected: false,
                                });
                                return AppMode::Partitions(view);
                            }
                        }
                        Err(message) => error = Some(message),
                    }
                } else {
                    let _ = edit_cursor_input(&mut input, &mut cursor, key);
                }
                view.overlay = Some(PartitionOverlay::FormatLabel {
                    filesystem,
                    encrypted,
                    input,
                    cursor,
                    error,
                });
                AppMode::Partitions(view)
            }
            PartitionOverlay::Input {
                task,
                mut input,
                mut cursor,
                hint,
                error: _,
            } => {
                let mut error = None;
                if key.code == KeyCode::Esc {
                    let tasks = self.partition_tasks_for_view(&view);
                    view.overlay = Some(PartitionOverlay::Actions {
                        selected: tasks
                            .iter()
                            .position(|candidate| *candidate == task)
                            .unwrap_or(0),
                    });
                    return AppMode::Partitions(view);
                }
                if key.code == KeyCode::Enter {
                    match self.partition_action_from_input(&view, task, &input) {
                        Ok(action) => {
                            if let Err(validation) =
                                partition::validate_snapshot(&action, &view.entries)
                            {
                                error = Some(validation.to_string());
                                view.overlay = Some(PartitionOverlay::Input {
                                    task,
                                    input,
                                    cursor,
                                    hint,
                                    error,
                                });
                                return AppMode::Partitions(view);
                            }
                            view.overlay = Some(PartitionOverlay::Confirm {
                                action,
                                yes_selected: false,
                            });
                            return AppMode::Partitions(view);
                        }
                        Err(message) => error = Some(message),
                    }
                } else {
                    let _ = edit_cursor_input(&mut input, &mut cursor, key);
                }
                view.overlay = Some(PartitionOverlay::Input {
                    task,
                    input,
                    cursor,
                    hint,
                    error,
                });
                AppMode::Partitions(view)
            }
            PartitionOverlay::Confirm {
                action,
                mut yes_selected,
            } => {
                match key.code {
                    KeyCode::Esc => {
                        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                        return AppMode::Partitions(view);
                    }
                    _ if hotkeys.confirm_no.matches(key) => {
                        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                        return AppMode::Partitions(view);
                    }
                    KeyCode::Left => yes_selected = false,
                    KeyCode::Right => yes_selected = true,
                    KeyCode::Tab | KeyCode::BackTab => yes_selected = !yes_selected,
                    _ if hotkeys.confirm_yes.matches(key) => {
                        return self.authorize_partition_operation(action, view);
                    }
                    KeyCode::Enter if yes_selected => {
                        return self.authorize_partition_operation(action, view);
                    }
                    KeyCode::Enter => {
                        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
                        return AppMode::Partitions(view);
                    }
                    _ => {}
                }
                view.overlay = Some(PartitionOverlay::Confirm {
                    action,
                    yes_selected,
                });
                AppMode::Partitions(view)
            }
        }
    }

    pub(crate) fn authorize_partition_operation(
        &mut self,
        action: PartitionAction,
        mut view: PartitionView,
    ) -> AppMode {
        view.overlay = None;
        if partition::authentication_required(&action) {
            AppMode::Prompt(Prompt::PartitionAuthentication {
                action,
                view,
                input: SecretInput::default(),
                error: None,
            })
        } else {
            self.start_partition_operation(action, view, None);
            AppMode::Progress
        }
    }

    pub(crate) fn begin_partition_task(
        &mut self,
        mut view: PartitionView,
        task: PartitionTask,
    ) -> AppMode {
        if view.entries.get(view.selected).is_none() {
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::Format {
            view.overlay = Some(PartitionOverlay::FormatOptions {
                selected: 0,
                encrypted: false,
            });
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::CreateTable {
            view.overlay = Some(PartitionOverlay::DiskLayoutOptions {
                selected: 0,
                overwrite: false,
            });
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::CreatePartition {
            view.overlay = Some(PartitionOverlay::FreeRegionOptions { selected: 0 });
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::EncryptionAccess {
            let selected_path = view.entries[view.selected].device.path.clone();
            match luks::discover() {
                Ok(devices) => {
                    if let Some(selected) = devices.iter().position(|device| {
                        device.source == selected_path
                            || device.mapping.as_ref() == Some(&selected_path)
                    }) {
                        return self.device_action(DeviceView { devices, selected });
                    }
                    self.set_notice("LUKS state changed · refresh and retry");
                }
                Err(error) => self.set_notice(error.to_string()),
            }
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::ChangePassphrase {
            view.overlay = Some(PartitionOverlay::ChangePassphrase {
                old: SecretInput::default(),
                new: SecretInput::default(),
                confirmation: SecretInput::default(),
                stage: 0,
                error: None,
            });
            return AppMode::Partitions(view);
        }
        if task == PartitionTask::Eject {
            let drive = view.entries[view.selected].device.path.clone();
            match luks::discover().ok().and_then(|devices| {
                devices.into_iter().find(|device| {
                    device.drive == drive && device.ejectable && !device.eject_blocked
                })
            }) {
                Some(device) => {
                    return AppMode::Prompt(Prompt::ConfirmLuks {
                        action: LuksAction::Eject {
                            source: device.source,
                            drive: device.drive,
                        },
                        title: "Eject device".into(),
                        body: "Active filesystems will be unmounted first.".into(),
                    })
                }
                None => self.set_notice("This drive cannot be safely ejected"),
            }
            return AppMode::Partitions(view);
        }
        if matches!(
            task,
            PartitionTask::SmartReport
                | PartitionTask::SmartShortTest
                | PartitionTask::SmartExtendedTest
        ) {
            return match self.partition_action_from_input(&view, task, "") {
                Ok(action) => {
                    if let Err(error) = partition::validate_snapshot(&action, &view.entries) {
                        self.set_notice(error.to_string());
                        AppMode::Partitions(view)
                    } else {
                        self.authorize_partition_operation(action, view)
                    }
                }
                Err(error) => {
                    self.set_notice(error);
                    AppMode::Partitions(view)
                }
            };
        }
        if matches!(
            task,
            PartitionTask::Mount
                | PartitionTask::Unmount
                | PartitionTask::Delete
                | PartitionTask::Check
                | PartitionTask::Repair
        ) {
            match self.partition_action_from_input(&view, task, "") {
                Ok(action) => {
                    if let Err(error) = partition::validate_snapshot(&action, &view.entries) {
                        self.set_notice(error.to_string());
                        return AppMode::Partitions(view);
                    }
                    view.overlay = Some(PartitionOverlay::Confirm {
                        action,
                        yes_selected: false,
                    });
                }
                Err(error) => self.set_notice(error),
            }
            return AppMode::Partitions(view);
        }
        let (input, hint) = self.partition_task_input(&view, task);
        view.overlay = Some(PartitionOverlay::Input {
            task,
            cursor: input.chars().count(),
            input,
            hint,
            error: None,
        });
        AppMode::Partitions(view)
    }

    pub(crate) fn partition_unmount_preflight(
        &mut self,
        view: &PartitionView,
        task: PartitionTask,
    ) -> Option<AppMode> {
        if matches!(
            task,
            PartitionTask::Mount
                | PartitionTask::Unmount
                | PartitionTask::EncryptionAccess
                | PartitionTask::MountOptions
                | PartitionTask::EncryptionOptions
                | PartitionTask::Eject
                | PartitionTask::Check
                | PartitionTask::CreateImage
                | PartitionTask::BackupTable
        ) {
            return None;
        }
        let selected = view.entries.get(view.selected)?;
        let selected_path = &selected.device.path;
        let belongs_to_selection = |entry: &PartitionEntry| {
            let mut path = Some(entry.device.path.as_path());
            while let Some(current) = path {
                if current == selected_path {
                    return true;
                }
                path = view
                    .entries
                    .iter()
                    .find(|candidate| candidate.device.path == current)
                    .and_then(|candidate| candidate.device.parent.as_deref());
            }
            false
        };
        let mut mounted = view
            .entries
            .iter()
            .filter(|entry| entry.device.is_mounted() && belongs_to_selection(entry))
            .collect::<Vec<_>>();
        if mounted.is_empty() {
            return None;
        }
        if mounted
            .iter()
            .any(|entry| entry.protected || entry.device.kind == "crypt")
        {
            return None;
        }
        mounted.sort_by_key(|entry| std::cmp::Reverse(entry.depth));
        let targets = mounted
            .iter()
            .map(|entry| entry.device.path.clone())
            .collect::<Vec<_>>();
        let details = mounted
            .iter()
            .map(|entry| {
                format!(
                    "{} · {}",
                    entry.device.path.display(),
                    entry
                        .device
                        .mountpoints
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let first = targets[0].clone();
        self.partition_preflight = Some(PendingPartitionPreflight {
            view: view.clone(),
            task,
            remaining: targets.into_iter().skip(1).collect(),
        });
        Some(AppMode::Prompt(Prompt::ConfirmLuks {
            action: LuksAction::UnmountFilesystem { source: first },
            title: "Unmount storage before continuing".into(),
            body: format!(
                "{} needs inactive storage. Unmount first:\n\n{}\n\nBusy devices stop safely. You will confirm the main action separately.",
                task.name(), details
            ),
        }))
    }

    pub(crate) fn partition_tasks_for_view(&self, view: &PartitionView) -> Vec<PartitionTask> {
        let Some(entry) = view.entries.get(view.selected) else {
            return Vec::new();
        };
        let luks_entry = entry.device.filesystem.as_deref() == Some("crypto_LUKS")
            || entry.device.kind == "crypt";
        if entry.device.is_disk() {
            let mut tasks = Vec::new();
            if luks_entry {
                tasks.push(PartitionTask::EncryptionAccess);
                if entry.device.filesystem.as_deref() == Some("crypto_LUKS") {
                    tasks.push(PartitionTask::ChangePassphrase);
                    tasks.push(PartitionTask::EncryptionOptions);
                }
            } else if entry.device.filesystem.is_some() {
                tasks.push(if entry.device.is_mounted() {
                    PartitionTask::Unmount
                } else {
                    PartitionTask::Mount
                });
            }
            if entry.device.table_type.is_some() {
                tasks.extend([PartitionTask::CreatePartition, PartitionTask::CreateTable]);
            } else {
                tasks.extend([PartitionTask::CreateTable, PartitionTask::Format]);
            }
            tasks.extend([
                PartitionTask::CreateImage,
                PartitionTask::RestoreImage,
                PartitionTask::SmartReport,
                PartitionTask::SmartShortTest,
                PartitionTask::SmartExtendedTest,
                PartitionTask::DriveSettings,
            ]);
            if entry.device.filesystem.is_some() && !luks_entry {
                tasks.push(PartitionTask::MountOptions);
            }
            tasks.push(PartitionTask::Eject);
            return tasks;
        }
        let mut tasks = Vec::new();
        if luks_entry {
            tasks.push(PartitionTask::EncryptionAccess);
            if entry.device.filesystem.as_deref() == Some("crypto_LUKS") {
                tasks.push(PartitionTask::ChangePassphrase);
                tasks.push(PartitionTask::EncryptionOptions);
            } else if entry.device.kind == "crypt" && entry.device.uuid.is_some() {
                tasks.push(PartitionTask::MountOptions);
            }
        } else if entry.device.filesystem.is_some() {
            tasks.push(if entry.device.is_mounted() {
                PartitionTask::Unmount
            } else {
                PartitionTask::Mount
            });
        }
        tasks.extend([
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
        ]);
        if entry.device.filesystem.is_some() && !luks_entry {
            tasks.push(PartitionTask::MountOptions);
        }
        tasks
    }

    pub(crate) fn partition_task_name(
        &self,
        view: &PartitionView,
        task: PartitionTask,
    ) -> &'static str {
        if task == PartitionTask::EncryptionAccess {
            if let Some(entry) = view.entries.get(view.selected) {
                if let Some(device) = luks::discover().ok().and_then(|devices| {
                    devices.into_iter().find(|device| {
                        device.source == entry.device.path
                            || device.mapping.as_ref() == Some(&entry.device.path)
                    })
                }) {
                    return if device.is_locked() {
                        "Unlock and mount"
                    } else if device.is_mounted() {
                        "Unmount and lock"
                    } else {
                        "Mount unlocked volume"
                    };
                }
            }
        }
        if task == PartitionTask::CreateTable {
            if let Some(entry) = view.entries.get(view.selected) {
                if entry.device.table_type.is_some() || entry.device.filesystem.is_some() {
                    return "Format disk";
                }
            }
            return "Format disk";
        }
        if task == PartitionTask::Format
            && view
                .entries
                .get(view.selected)
                .is_some_and(|entry| entry.device.is_disk() && entry.device.table_type.is_none())
        {
            return "Use whole disk";
        }
        task.name()
    }

    pub(crate) fn partition_task_description(
        &self,
        view: &PartitionView,
        task: PartitionTask,
    ) -> &'static str {
        if task == PartitionTask::CreateTable {
            if view
                .entries
                .get(view.selected)
                .is_some_and(|entry| entry.device.table_type.is_none())
            {
                return "Erase current content and choose GPT/MBR";
            }
            return "Leave empty or create GPT/MBR";
        }
        if task == PartitionTask::Format
            && view
                .entries
                .get(view.selected)
                .is_some_and(|entry| entry.device.is_disk() && entry.device.table_type.is_none())
        {
            return "Create one filesystem without partitions";
        }
        task.description()
    }

    pub(crate) fn partition_task_unavailable(
        &self,
        view: &PartitionView,
        task: PartitionTask,
    ) -> Option<String> {
        let entry = view.entries.get(view.selected)?;
        let changes_storage = !matches!(
            task,
            PartitionTask::Check
                | PartitionTask::BackupTable
                | PartitionTask::CreateImage
                | PartitionTask::SmartReport
                | PartitionTask::SmartShortTest
                | PartitionTask::SmartExtendedTest
        );
        if self.config.behavior.read_only && changes_storage {
            return Some("Read-only mode: partition operations are disabled".into());
        }
        if entry.protected && changes_storage {
            return Some("Protected system storage cannot be modified".into());
        }
        if entry.device.read_only && changes_storage {
            return Some("The kernel reports this device as read only".into());
        }
        let active = entry.device.is_mounted() || entry.mounted_descendants;
        match task {
            PartitionTask::EncryptionAccess => {
                let found = luks::discover().ok().is_some_and(|devices| {
                    devices.iter().any(|device| {
                        device.source == entry.device.path
                            || device.mapping.as_ref() == Some(&entry.device.path)
                    })
                });
                (!found).then(|| "LUKS state is unavailable".into())
            }
            PartitionTask::ChangePassphrase => {
                if entry.device.filesystem.as_deref() != Some("crypto_LUKS") {
                    Some("Select a locked LUKS volume".into())
                } else if active {
                    Some("Unmount and lock the volume first".into())
                } else {
                    None
                }
            }
            PartitionTask::MountOptions => {
                if entry.device.uuid.as_deref().is_none_or(str::is_empty) {
                    Some("This filesystem has no UUID".into())
                } else {
                    None
                }
            }
            PartitionTask::EncryptionOptions => {
                if entry.device.filesystem.as_deref() != Some("crypto_LUKS") {
                    Some("Select a LUKS volume".into())
                } else if entry.device.uuid.as_deref().is_none_or(str::is_empty) {
                    Some("This LUKS volume has no UUID".into())
                } else {
                    None
                }
            }
            PartitionTask::Eject => {
                let found = luks::discover().ok().is_some_and(|devices| {
                    devices.iter().any(|device| {
                        device.drive == entry.device.path
                            && device.ejectable
                            && !device.eject_blocked
                    })
                });
                (!found).then(|| "This drive cannot be safely ejected".into())
            }
            PartitionTask::SmartReport
            | PartitionTask::SmartShortTest
            | PartitionTask::SmartExtendedTest => {
                if !entry.device.is_disk() {
                    Some("Select a whole disk".into())
                } else if !partition::helper_available("smartctl") {
                    Some("Install smartmontools to use SMART".into())
                } else {
                    None
                }
            }
            PartitionTask::DriveSettings => {
                if !entry.device.is_disk() {
                    Some("Select a whole disk".into())
                } else if entry.device.transport.as_deref() == Some("nvme") {
                    Some("ATA drive settings are unavailable for this disk".into())
                } else if !partition::helper_available("hdparm") {
                    Some("Install hdparm to change drive settings".into())
                } else {
                    None
                }
            }
            PartitionTask::Mount => {
                if entry.device.filesystem.is_none()
                    || entry.device.filesystem.as_deref() == Some("crypto_LUKS")
                {
                    Some("Select a filesystem".into())
                } else if entry.device.is_mounted() {
                    Some("Already mounted".into())
                } else {
                    None
                }
            }
            PartitionTask::Unmount => {
                if !entry.device.is_mounted() {
                    Some("Not mounted".into())
                } else {
                    None
                }
            }
            PartitionTask::CreatePartition => {
                if !entry.device.is_disk() {
                    Some("Select a whole disk".into())
                } else if active {
                    Some("The disk contains mounted storage".into())
                } else if entry.device.table_type.is_none() {
                    Some("Create a GPT or MBR partition table first".into())
                } else if partition::largest_free_region(entry, &view.entries).is_none() {
                    Some("No usable free space was found".into())
                } else {
                    None
                }
            }
            PartitionTask::CreateTable => {
                if !entry.device.is_disk() {
                    Some("Select a whole disk".into())
                } else if active {
                    Some("The disk contains mounted storage".into())
                } else {
                    None
                }
            }
            PartitionTask::Resize => {
                if entry.device.kind != "part" {
                    Some("Select a partition".into())
                } else if active {
                    Some("Unmount the partition before resizing it".into())
                } else if entry.device.filesystem.as_deref() != Some("ext4") {
                    Some("Safe resizing currently supports ext4 only".into())
                } else if entry.device.partition_number.is_none() || entry.device.parent.is_none() {
                    Some("Partition boundary information is unavailable".into())
                } else {
                    None
                }
            }
            PartitionTask::Format => {
                if active {
                    Some("Unmount the device before modifying it".into())
                } else if entry.device.is_disk() && entry.device.table_type.is_some() {
                    Some("Select a partition to format it".into())
                } else {
                    None
                }
            }
            PartitionTask::Delete
            | PartitionTask::Label
            | PartitionTask::Check
            | PartitionTask::Repair
            | PartitionTask::Flag
            | PartitionTask::PartitionName
            | PartitionTask::PartitionType => {
                if entry.device.kind != "part" {
                    Some("Select a partition".into())
                } else if active {
                    Some("Unmount the partition before modifying it".into())
                } else if task == PartitionTask::Label
                    && entry
                        .device
                        .filesystem
                        .as_deref()
                        .and_then(Filesystem::parse)
                        .is_none()
                {
                    Some("This filesystem does not support label editing".into())
                } else if task == PartitionTask::Check
                    && !entry
                        .device
                        .filesystem
                        .as_deref()
                        .and_then(Filesystem::parse)
                        .is_some_and(|filesystem| filesystem != Filesystem::Swap)
                {
                    Some("This filesystem does not support a read-only check".into())
                } else if task == PartitionTask::PartitionName
                    && entry
                        .device
                        .parent
                        .as_ref()
                        .and_then(|parent| {
                            view.entries.iter().find(|candidate| {
                                candidate.device.path == *parent && candidate.device.is_disk()
                            })
                        })
                        .and_then(|disk| disk.device.table_type.as_deref())
                        != Some("gpt")
                {
                    Some("Partition names require GPT".into())
                } else {
                    None
                }
            }
            PartitionTask::BackupTable => {
                if !entry.device.is_disk() || entry.device.table_type.is_none() {
                    Some("Select a disk with a partition table".into())
                } else {
                    None
                }
            }
            PartitionTask::CreateImage => None,
            PartitionTask::RestoreImage => {
                active.then(|| "Unmount storage before restoring".into())
            }
        }
    }

    pub(crate) fn partition_task_input(
        &self,
        _view: &PartitionView,
        task: PartitionTask,
    ) -> (String, String) {
        match task {
            PartitionTask::Mount
            | PartitionTask::Unmount
            | PartitionTask::EncryptionAccess
            | PartitionTask::ChangePassphrase
            | PartitionTask::Eject => (String::new(), String::new()),
            PartitionTask::SmartReport
            | PartitionTask::SmartShortTest
            | PartitionTask::SmartExtendedTest => (String::new(), String::new()),
            PartitionTask::DriveSettings => (
                "write-cache on".into(),
                "standby 0-255 · apm 1-255 · aam 128-254 · write-cache on/off".into(),
            ),
            PartitionTask::MountOptions => (
                _view
                    .entries
                    .get(_view.selected)
                    .and_then(|entry| entry.device.uuid.as_deref())
                    .and_then(partition::current_mount_options)
                    .map(|(mountpoint, options)| format!("{} {options}", mountpoint.display()))
                    .unwrap_or_else(|| "/mnt/data defaults,nofail".into()),
                "Absolute mount point, then comma-separated options".into(),
            ),
            PartitionTask::EncryptionOptions => (
                _view
                    .entries
                    .get(_view.selected)
                    .and_then(|entry| entry.device.uuid.as_deref())
                    .and_then(partition::current_encryption_options)
                    .map(|(name, options)| format!("{name} {options}"))
                    .unwrap_or_else(|| "encrypted-volume nofail".into()),
                "Mapping name, then comma-separated crypttab options".into(),
            ),
            PartitionTask::CreatePartition => (
                "max".into(),
                "Size: max (recommended), a percentage, or a value such as 20GiB".into(),
            ),
            PartitionTask::Resize => {
                let current = _view
                    .entries
                    .get(_view.selected)
                    .map(|entry| partition::size_input(entry.device.size))
                    .unwrap_or_else(|| "max".into());
                (
                    current,
                    "Final size: max, a percentage, or a value such as 20GiB".into(),
                )
            }
            PartitionTask::Format => (
                "ext4".into(),
                format!(
                    "Filesystem: {}. Add an optional label after it",
                    Filesystem::NAMES
                ),
            ),
            PartitionTask::CreateTable => (
                "gpt".into(),
                "Partition table: gpt (recommended) or mbr".into(),
            ),
            PartitionTask::Delete | PartitionTask::Check | PartitionTask::Repair => {
                (String::new(), String::new())
            }
            PartitionTask::Label => (
                _view
                    .entries
                    .get(_view.selected)
                    .and_then(|entry| entry.device.label.clone())
                    .unwrap_or_default(),
                "New filesystem label".into(),
            ),
            PartitionTask::Flag => (
                "boot on".into(),
                "Flag and state: boot on, esp on, hidden off, lvm on, raid on".into(),
            ),
            PartitionTask::PartitionName => (
                _view
                    .entries
                    .get(_view.selected)
                    .and_then(|entry| entry.device.partition_label.clone())
                    .unwrap_or_default(),
                "New GPT partition name; leave blank to clear it".into(),
            ),
            PartitionTask::PartitionType => (
                _view
                    .entries
                    .get(_view.selected)
                    .and_then(|entry| entry.device.partition_type.clone())
                    .unwrap_or_default(),
                "Common type: linux, swap, efi, data, lvm, raid, or bios-boot".into(),
            ),
            PartitionTask::BackupTable => (
                self.current_dir
                    .join(format!(
                        "minfm-{}-partition-table.sfdisk",
                        _view
                            .entries
                            .get(_view.selected)
                            .map(|entry| entry.device.name())
                            .unwrap_or_else(|| "disk".into())
                    ))
                    .display()
                    .to_string(),
                "New backup file path; existing files are never overwritten".into(),
            ),
            PartitionTask::CreateImage => (
                self.current_dir
                    .join(format!(
                        "{}.img",
                        _view
                            .entries
                            .get(_view.selected)
                            .map(|entry| entry.device.name())
                            .unwrap_or_else(|| "disk".into())
                    ))
                    .display()
                    .to_string(),
                "New image file".into(),
            ),
            PartitionTask::RestoreImage => (String::new(), "Existing image file".into()),
        }
    }

    pub(crate) fn partition_action_from_input(
        &self,
        view: &PartitionView,
        task: PartitionTask,
        input: &str,
    ) -> std::result::Result<PartitionAction, String> {
        let entry = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No device is selected".to_string())?;
        let target = DeviceIdentity::from_entry(entry).map_err(|error| error.to_string())?;
        match task {
            PartitionTask::Mount => Ok(PartitionAction::Mount { target }),
            PartitionTask::Unmount => Ok(PartitionAction::Unmount { target }),
            PartitionTask::EncryptionAccess | PartitionTask::Eject => {
                Err("This device action uses the safe device workflow".into())
            }
            PartitionTask::ChangePassphrase => {
                Err("This action uses the protected passphrase form".into())
            }
            PartitionTask::MountOptions => {
                let mut values = input.split_whitespace();
                let mountpoint = values
                    .next()
                    .map(PathBuf::from)
                    .ok_or("Enter a mount point")?;
                let options = values.next().ok_or("Enter mount options")?.to_owned();
                if values.next().is_some() {
                    return Err("Enter a mount point and one options list".into());
                }
                let uuid = entry
                    .device
                    .uuid
                    .clone()
                    .ok_or("This filesystem has no UUID")?;
                Ok(PartitionAction::SetMountOptions {
                    target,
                    uuid,
                    mountpoint,
                    options,
                })
            }
            PartitionTask::EncryptionOptions => {
                let mut values = input.split_whitespace();
                let name = values.next().ok_or("Enter a mapping name")?.to_owned();
                let options = values.next().ok_or("Enter encryption options")?.to_owned();
                if values.next().is_some() {
                    return Err("Enter a mapping name and one options list".into());
                }
                let uuid = entry
                    .device
                    .uuid
                    .clone()
                    .ok_or("This LUKS volume has no UUID")?;
                Ok(PartitionAction::SetEncryptionOptions {
                    target,
                    uuid,
                    name,
                    options,
                })
            }
            PartitionTask::SmartReport => Ok(PartitionAction::SmartReport { disk: target }),
            PartitionTask::SmartShortTest => Ok(PartitionAction::SmartTest {
                disk: target,
                extended: false,
            }),
            PartitionTask::SmartExtendedTest => Ok(PartitionAction::SmartTest {
                disk: target,
                extended: true,
            }),
            PartitionTask::DriveSettings => {
                let mut values = input.split_whitespace();
                let name = values.next().unwrap_or_default();
                let value = values.next().unwrap_or_default();
                if values.next().is_some() {
                    return Err("Enter one setting and one value".into());
                }
                let setting = match name {
                    "standby" => partition::DriveSetting::Standby(
                        value.parse().map_err(|_| "Standby must be 0-255")?,
                    ),
                    "apm" => partition::DriveSetting::PowerManagement(
                        value.parse().map_err(|_| "APM must be 1-255")?,
                    ),
                    "aam" => partition::DriveSetting::AcousticManagement(
                        value.parse().map_err(|_| "AAM must be 128-254")?,
                    ),
                    "write-cache" => partition::DriveSetting::WriteCache(match value {
                        "on" => true,
                        "off" => false,
                        _ => return Err("Write cache must be on or off".into()),
                    }),
                    _ => return Err("Use standby, apm, aam, or write-cache".into()),
                };
                Ok(PartitionAction::DriveSetting {
                    disk: target,
                    setting,
                })
            }
            PartitionTask::CreatePartition => {
                let (start_bytes, maximum_end) =
                    partition::largest_free_region(entry, &view.entries)
                        .ok_or_else(|| "No usable free space was found".to_string())?;
                let available = maximum_end.saturating_sub(start_bytes);
                let requested = input.trim();
                let requested_size = if requested.eq_ignore_ascii_case("max") {
                    available
                } else {
                    partition::parse_size(requested, available)
                        .map_err(|error| error.to_string())?
                };
                if requested_size == 0 || requested_size > available {
                    return Err(format!(
                        "Size must be between 1 byte and {}",
                        partition::size_input(available)
                    ));
                }
                let sector = entry.device.logical_sector_size.max(1);
                let requested_end = start_bytes.saturating_add(requested_size);
                let end_bytes = requested_end - requested_end % sector;
                if end_bytes <= start_bytes {
                    return Err(format!("Size must be at least {sector} bytes"));
                }
                Ok(PartitionAction::CreatePartition {
                    disk: target,
                    start_bytes,
                    end_bytes,
                })
            }
            PartitionTask::Resize => self.partition_resize_action(view, input),
            PartitionTask::Format => {
                let mut values = input.splitn(2, char::is_whitespace);
                let filesystem = values
                    .next()
                    .and_then(Filesystem::parse)
                    .ok_or_else(|| format!("Supported filesystems: {}", Filesystem::NAMES))?;
                let label = values
                    .next()
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(str::to_owned);
                Ok(PartitionAction::Format {
                    target,
                    filesystem,
                    label,
                })
            }
            PartitionTask::CreateTable => {
                let table = PartitionTable::parse(input)
                    .ok_or_else(|| "Partition table must be gpt or msdos/mbr".to_string())?;
                Ok(PartitionAction::CreateTable {
                    disk: target,
                    table,
                    overwrite: false,
                })
            }
            PartitionTask::Delete => {
                let (disk, number) = self.partition_parent_context(view)?;
                Ok(PartitionAction::DeletePartition {
                    target,
                    disk,
                    number,
                })
            }
            PartitionTask::Label => {
                let filesystem = entry
                    .device
                    .filesystem
                    .as_deref()
                    .and_then(Filesystem::parse)
                    .ok_or_else(|| "The filesystem does not support label editing".to_string())?;
                Ok(PartitionAction::SetLabel {
                    target,
                    filesystem,
                    label: input.trim().to_owned(),
                })
            }
            PartitionTask::Check => {
                let filesystem = entry
                    .device
                    .filesystem
                    .as_deref()
                    .and_then(Filesystem::parse)
                    .ok_or_else(|| {
                        "The filesystem does not support read-only checks".to_string()
                    })?;
                if filesystem == Filesystem::Swap {
                    return Err("Swap does not have a read-only filesystem check".into());
                }
                Ok(PartitionAction::CheckFilesystem { target, filesystem })
            }
            PartitionTask::Repair => {
                let filesystem = entry
                    .device
                    .filesystem
                    .as_deref()
                    .and_then(Filesystem::parse)
                    .ok_or_else(|| "This filesystem cannot be repaired".to_string())?;
                if matches!(filesystem, Filesystem::Swap | Filesystem::None) {
                    return Err("This filesystem cannot be repaired".into());
                }
                Ok(PartitionAction::RepairFilesystem { target, filesystem })
            }
            PartitionTask::Flag => {
                let (disk, number) = self.partition_parent_context(view)?;
                let mut values = input.split_whitespace();
                let flag = values
                    .next()
                    .ok_or_else(|| "Enter a flag and on/off".to_string())?;
                let enabled = match values.next() {
                    Some(value) if value.eq_ignore_ascii_case("on") => true,
                    Some(value) if value.eq_ignore_ascii_case("off") => false,
                    _ => return Err("Flag state must be on or off".into()),
                };
                if values.next().is_some() {
                    return Err("Enter one flag followed by on or off".into());
                }
                Ok(PartitionAction::SetFlag {
                    target,
                    disk,
                    number,
                    flag: flag.to_ascii_lowercase(),
                    enabled,
                })
            }
            PartitionTask::PartitionName => {
                let (disk, number) = self.partition_parent_context(view)?;
                Ok(PartitionAction::SetPartitionName {
                    target,
                    disk,
                    number,
                    name: input.trim().to_owned(),
                })
            }
            PartitionTask::PartitionType => {
                let (disk, number) = self.partition_parent_context(view)?;
                Ok(PartitionAction::SetPartitionType {
                    target,
                    disk,
                    number,
                    type_id: self.partition_type_id(view, input)?,
                })
            }
            PartitionTask::BackupTable => Ok(PartitionAction::BackupTable {
                disk: target,
                destination: PathBuf::from(input.trim()),
            }),
            PartitionTask::CreateImage => Ok(PartitionAction::CreateImage {
                target,
                destination: PathBuf::from(input.trim()),
            }),
            PartitionTask::RestoreImage => Ok(PartitionAction::RestoreImage {
                target,
                source: PathBuf::from(input.trim()),
            }),
        }
    }

    pub(crate) fn partition_parent_context(
        &self,
        view: &PartitionView,
    ) -> std::result::Result<(DeviceIdentity, u32), String> {
        let partition = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No partition is selected".to_string())?;
        let parent = partition
            .device
            .parent
            .as_ref()
            .ok_or_else(|| "Parent disk information is unavailable".to_string())?;
        let disk = view
            .entries
            .iter()
            .find(|entry| entry.device.path == *parent && entry.device.is_disk())
            .ok_or_else(|| "Parent disk is unavailable".to_string())?;
        let number = partition
            .device
            .partition_number
            .ok_or_else(|| "Partition number is unavailable".to_string())?;
        Ok((
            DeviceIdentity::from_entry(disk).map_err(|error| error.to_string())?,
            number,
        ))
    }

    pub(crate) fn partition_type_id(
        &self,
        view: &PartitionView,
        input: &str,
    ) -> std::result::Result<String, String> {
        let partition = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No partition is selected".to_string())?;
        let table = partition
            .device
            .parent
            .as_ref()
            .and_then(|parent| {
                view.entries
                    .iter()
                    .find(|entry| entry.device.path == *parent && entry.device.is_disk())
            })
            .and_then(|disk| disk.device.table_type.as_deref())
            .ok_or_else(|| "Partition table type is unavailable".to_string())?;
        let value = input.trim().to_ascii_lowercase();
        let normalized =
            if table == "gpt" {
                match value.as_str() {
                    "linux" => "0fc63daf-8483-4772-8e79-3d69d8477de4",
                    "swap" => "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f",
                    "efi" | "esp" => "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
                    "data" | "windows" => "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
                    "lvm" => "e6d6d379-f507-44c2-a23c-238f2a3df928",
                    "raid" => "a19d880f-05fc-4d3b-a006-743f0f84911e",
                    "bios-boot" | "bios_grub" => "21686148-6449-6e6f-744e-656564454649",
                    _ if is_guid(&value) => return Ok(value),
                    _ => return Err(
                        "Use linux, swap, efi, data, lvm, raid, bios-boot, or an exact GPT GUID"
                            .into(),
                    ),
                }
            } else if matches!(table, "dos" | "msdos" | "mbr") {
                match value.as_str() {
                    "linux" => "83",
                    "swap" => "82",
                    "efi" | "esp" => "ef",
                    "data" | "windows" => "07",
                    "lvm" => "8e",
                    "raid" => "fd",
                    _ if is_mbr_type(&value) => return Ok(value),
                    _ => {
                        return Err(
                            "Use linux, swap, efi, data, lvm, raid, or a two-digit MBR ID".into(),
                        )
                    }
                }
            } else {
                return Err("Only GPT and MBR partition types are supported".into());
            };
        Ok(normalized.into())
    }

    pub(crate) fn partition_format_action(
        &self,
        view: &PartitionView,
        filesystem: Filesystem,
        label: &str,
    ) -> std::result::Result<PartitionAction, String> {
        let entry = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No device is selected".to_string())?;
        let target = DeviceIdentity::from_entry(entry).map_err(|error| error.to_string())?;
        let label = (!label.trim().is_empty()).then(|| label.trim().to_owned());
        Ok(PartitionAction::Format {
            target,
            filesystem,
            label,
        })
    }

    pub(crate) fn partition_encryption_action(
        &self,
        view: &PartitionView,
        filesystem: Filesystem,
        whole_disk: bool,
        label: Option<String>,
        passphrase: SecretInput,
    ) -> std::result::Result<PartitionAction, String> {
        let entry = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No device is selected".to_string())?;
        let identity = DeviceIdentity::from_entry(entry).map_err(|error| error.to_string())?;
        if whole_disk {
            if !entry.device.is_disk() {
                return Err("Select a whole disk for an encrypted disk layout".into());
            }
            Ok(PartitionAction::CreateEncryptedDisk {
                disk: identity,
                filesystem,
                label,
                passphrase,
            })
        } else {
            Ok(PartitionAction::EncryptFormat {
                target: identity,
                filesystem,
                label,
                passphrase,
            })
        }
    }

    pub(crate) fn partition_create_action_for_region(
        &self,
        view: &PartitionView,
        start_bytes: u64,
        maximum_end: u64,
        input: &str,
    ) -> std::result::Result<PartitionAction, String> {
        let disk = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No disk is selected".to_string())?;
        let identity = DeviceIdentity::from_entry(disk).map_err(|error| error.to_string())?;
        let available = maximum_end.saturating_sub(start_bytes);
        let requested = input.trim();
        let requested_size = if requested.eq_ignore_ascii_case("max") {
            available
        } else {
            partition::parse_size(requested, available).map_err(|error| error.to_string())?
        };
        if requested_size == 0 || requested_size > available {
            return Err(format!(
                "Size must be between 1 byte and {}",
                partition::size_input(available)
            ));
        }
        let sector = disk.device.logical_sector_size.max(1024 * 1024);
        let requested_end = start_bytes.saturating_add(requested_size);
        let end_bytes = requested_end - requested_end % sector;
        if end_bytes <= start_bytes {
            return Err(format!("Size must be at least {sector} bytes"));
        }
        Ok(PartitionAction::CreatePartition {
            disk: identity,
            start_bytes,
            end_bytes,
        })
    }

    pub(crate) fn partition_resize_action(
        &self,
        view: &PartitionView,
        input: &str,
    ) -> std::result::Result<PartitionAction, String> {
        let partition_entry = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No partition is selected".to_string())?;
        let parent = partition_entry
            .device
            .parent
            .as_ref()
            .ok_or_else(|| "Parent disk information is unavailable".to_string())?;
        let disk_entry = view
            .entries
            .iter()
            .find(|entry| entry.device.path == *parent && entry.device.is_disk())
            .ok_or_else(|| "Parent disk is unavailable".to_string())?;
        let start = partition_entry
            .device
            .start_bytes()
            .ok_or_else(|| "Partition start is unavailable".to_string())?;
        let current_end = partition_entry
            .device
            .end_bytes()
            .ok_or_else(|| "Partition end is unavailable".to_string())?;
        let maximum_end =
            partition::maximum_growth_end(partition_entry, &view.entries).unwrap_or(current_end);
        let maximum_size = maximum_end.saturating_sub(start);
        let requested_size = if input.trim().eq_ignore_ascii_case("max") {
            maximum_size
        } else {
            partition::parse_size(input.trim(), maximum_size).map_err(|error| error.to_string())?
        };
        let sector = disk_entry.device.logical_sector_size.max(1);
        let requested_end = start.saturating_add(requested_size);
        let end_bytes = requested_end - requested_end % sector;
        if end_bytes == current_end {
            return Err("The requested size is unchanged".into());
        }
        let target =
            DeviceIdentity::from_entry(partition_entry).map_err(|error| error.to_string())?;
        let disk = DeviceIdentity::from_entry(disk_entry).map_err(|error| error.to_string())?;
        let number = partition_entry
            .device
            .partition_number
            .ok_or_else(|| "Partition number is unavailable".to_string())?;
        if end_bytes > current_end {
            Ok(PartitionAction::Grow {
                target,
                disk,
                number,
                end_bytes,
                filesystem: Some(Filesystem::Ext4),
            })
        } else {
            Ok(PartitionAction::Shrink {
                target,
                disk,
                number,
                end_bytes,
                filesystem: Filesystem::Ext4,
            })
        }
    }

    pub(crate) fn partition_disk_layout_action(
        &self,
        view: &PartitionView,
        selected: usize,
        overwrite: bool,
    ) -> std::result::Result<PartitionAction, String> {
        let entry = view
            .entries
            .get(view.selected)
            .ok_or_else(|| "No disk is selected".to_string())?;
        let disk = DeviceIdentity::from_entry(entry).map_err(|error| error.to_string())?;
        match selected {
            0 => Ok(PartitionAction::EraseDisk { disk, overwrite }),
            1 => Ok(PartitionAction::CreateTable {
                disk,
                table: PartitionTable::Gpt,
                overwrite,
            }),
            2 => Ok(PartitionAction::CreateTable {
                disk,
                table: PartitionTable::Msdos,
                overwrite,
            }),
            _ => Err("Choose Empty, GPT, or MBR".into()),
        }
    }

    pub(super) fn start_luks(&mut self, action: LuksAction, retry: Option<LuksRetry>) {
        let (label, current) = match &action {
            LuksAction::UnlockAndMount { source, .. } => {
                ("Unlocking and mounting volume", source.clone())
            }
            LuksAction::Mount { mapping } => ("Mounting volume", mapping.clone()),
            LuksAction::MountFilesystem { source } => ("Mounting filesystem", source.clone()),
            LuksAction::UnmountFilesystem { source } => ("Unmounting filesystem", source.clone()),
            LuksAction::UnmountAndLock { source, .. } => {
                ("Unmounting and locking volume", source.clone())
            }
            LuksAction::Eject { drive, .. } => ("Safely ejecting device", drive.clone()),
        };
        let started_at = Instant::now();
        self.progress = ProgressState {
            label: label.into(),
            phase: Some("Preparing device operation".into()),
            current: Some(current),
            cancellable: false,
            started_at: Some(started_at),
            phase_started_at: Some(started_at),
            ..ProgressState::default()
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = luks::execute_with_progress(&action, |label| {
                let _ = sender.send(LuksUpdate::Phase {
                    label,
                    started_at: Instant::now(),
                });
            });
            let _ = sender.send(LuksUpdate::Finished(result));
        });
        self.luks_operation = Some(RunningLuks {
            receiver,
            retry,
            started_at,
        });
    }
    #[allow(dead_code)]
    pub(crate) fn open_devices(&mut self) -> AppMode {
        if cfg!(test) {
            self.last_device_refresh = Instant::now();
            return match luks::discover() {
                Ok(devices) => AppMode::Devices(DeviceView {
                    devices,
                    selected: 0,
                }),
                Err(error) => AppMode::Prompt(Prompt::Message {
                    title: "Storage devices unavailable".into(),
                    body: error.to_string(),
                }),
            };
        }
        self.start_device_refresh(None);
        AppMode::Devices(DeviceView {
            devices: Vec::new(),
            selected: 0,
        })
    }

    pub(crate) fn open_partitions(&mut self, return_to_apps: bool) -> AppMode {
        if !self.device_manager_available() {
            return AppMode::Prompt(Prompt::Message {
                title: "Device Manager unavailable".into(),
                body: "Device discovery cannot start because the lsblk command is unavailable. Install util-linux, then try again.".into(),
            });
        }
        self.partition_return_to_apps = return_to_apps;
        self.start_partition_refresh(None);
        AppMode::Partitions(PartitionView {
            entries: Vec::new(),
            selected: 0,
            overlay: None,
        })
    }

    pub(crate) fn reopen_partitions(&mut self) -> AppMode {
        self.open_partitions(self.partition_return_to_apps)
    }

    pub(crate) fn partition_returns_to_apps(&self) -> bool {
        self.partition_return_to_apps
    }

    pub(crate) fn device_manager_available(&self) -> bool {
        command_available("lsblk")
    }
}
