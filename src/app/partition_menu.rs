use super::*;

impl App {
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
            self.manager_return_mode()
        } else if hotkeys.tools.matches(key) {
            AppMode::Tools(ToolsView { selected: 0 })
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
                permissions: crate::partition::FilesystemPermissions::SystemDefault,
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

    pub(crate) fn open_partitions(&mut self, manager_return: ManagerReturn) -> AppMode {
        self.manager_return = manager_return;
        if !self.device_manager_available() {
            self.modal_return = match manager_return {
                ManagerReturn::Files => ReturnDestination::Browser,
                ManagerReturn::Tools => ReturnDestination::Tools,
            };
            return AppMode::Prompt(Prompt::Message {
                title: "Device Manager unavailable".into(),
                body: "Device discovery cannot start because the lsblk command is unavailable. Install util-linux, then try again.".into(),
            });
        }
        self.start_partition_refresh(None);
        AppMode::Partitions(PartitionView {
            entries: Vec::new(),
            selected: 0,
            overlay: None,
        })
    }

    pub(crate) fn reopen_partitions(&mut self) -> AppMode {
        self.open_partitions(self.manager_return)
    }

    pub(crate) fn manager_returns_to_tools(&self) -> bool {
        self.manager_return == ManagerReturn::Tools
    }

    pub(crate) fn manager_return_mode(&self) -> AppMode {
        match self.manager_return {
            ManagerReturn::Files => AppMode::Browser,
            ManagerReturn::Tools => AppMode::Tools(ToolsView { selected: 0 }),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_manager_return_for_test(&mut self, manager_return: ManagerReturn) {
        self.manager_return = manager_return;
    }
}
