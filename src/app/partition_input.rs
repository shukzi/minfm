use super::*;
use crate::partition::FilesystemPermissions;

impl App {
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
                mut permissions,
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
                    permissions = permissions.effective_for(Filesystem::ALL[selected]);
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                        permissions,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    permissions = permissions.effective_for(Filesystem::ALL[selected]);
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                        permissions,
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
                        permissions,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Char('p') => {
                    if Filesystem::ALL[selected].supports_unix_ownership() {
                        permissions = match permissions {
                            FilesystemPermissions::SystemDefault => FilesystemPermissions::Everyone,
                            FilesystemPermissions::Everyone => FilesystemPermissions::SystemDefault,
                        };
                    }
                    view.overlay = Some(PartitionOverlay::FormatOptions {
                        selected,
                        encrypted,
                        permissions,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Enter | KeyCode::Right => {
                    let Some(filesystem) = Filesystem::ALL.get(selected).copied() else {
                        view.overlay = Some(PartitionOverlay::FormatOptions {
                            selected,
                            encrypted,
                            permissions,
                        });
                        return AppMode::Partitions(view);
                    };
                    view.overlay = Some(PartitionOverlay::FormatLabel {
                        filesystem,
                        encrypted,
                        permissions,
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
                        permissions,
                    });
                    AppMode::Partitions(view)
                }
            },
            PartitionOverlay::EncryptionFilesystem {
                mut selected,
                whole_disk,
                permissions,
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
                            permissions,
                        }
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(Filesystem::ALL.len() - 1);
                    view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                        selected,
                        whole_disk,
                        permissions,
                    });
                    AppMode::Partitions(view)
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    view.overlay = Some(PartitionOverlay::EncryptionFilesystem {
                        selected,
                        whole_disk,
                        permissions,
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
                        permissions,
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
                        permissions,
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
                permissions,
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
                            permissions,
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
                                permissions,
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
                    permissions,
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
                permissions,
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
                        permissions,
                    });
                    return AppMode::Partitions(view);
                }
                let mut error = None;
                if key.code == KeyCode::Enter {
                    if encrypted {
                        view.overlay = Some(PartitionOverlay::EncryptionPassphrase {
                            filesystem,
                            whole_disk: false,
                            permissions,
                            label: (!input.trim().is_empty()).then(|| input.trim().to_owned()),
                            passphrase: SecretInput::default(),
                            confirmation: SecretInput::default(),
                            confirming: false,
                            error: None,
                        });
                        return AppMode::Partitions(view);
                    }
                    match self.partition_format_action(&view, filesystem, &input, permissions) {
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
                    permissions,
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
                    permissions: FilesystemPermissions::SystemDefault,
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
        permissions: FilesystemPermissions,
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
            permissions: permissions.effective_for(filesystem),
        })
    }

    pub(crate) fn partition_encryption_action(
        &self,
        view: &PartitionView,
        filesystem: Filesystem,
        whole_disk: bool,
        label: Option<String>,
        passphrase: SecretInput,
        permissions: FilesystemPermissions,
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
                permissions: permissions.effective_for(filesystem),
            })
        } else {
            Ok(PartitionAction::EncryptFormat {
                target: identity,
                filesystem,
                label,
                passphrase,
                permissions: permissions.effective_for(filesystem),
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
}
