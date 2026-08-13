use super::*;

impl App {
    pub(crate) fn visible_status(&self) -> &str {
        match &self.status_expiry {
            Some((message, deadline)) if message == &self.status && Instant::now() >= *deadline => {
                ""
            }
            _ => &self.status,
        }
    }

    pub(crate) fn set_notice(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_expiry = Some((self.status.clone(), Instant::now() + STATUS_NOTICE_DURATION));
    }

    pub(crate) fn poll_browser_load(&mut self) -> bool {
        let Some(running) = &self.browser_load else {
            return false;
        };
        let mut updates = Vec::new();
        let mut disconnected = false;
        for _ in 0..64 {
            match running.receiver.try_recv() {
                Ok(update) => updates.push(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if updates.is_empty() && !disconnected {
            return false;
        }

        let active_generation = running.generation;
        let mut finished = disconnected;
        let mut received_finished = false;
        for update in updates {
            match update {
                LoadUpdate::Batch {
                    generation,
                    entries,
                    depths,
                } if generation == self.browser_generation => {
                    if self.browser_loaded_entries == 0 {
                        self.entries.clear();
                        self.tree_depths.clear();
                        self.cursor = 0;
                    }
                    self.browser_loaded_entries += entries.len();
                    self.entries.extend(entries);
                    if self.browser_view == BrowserView::Tree {
                        self.tree_depths.extend(depths);
                    }
                }
                LoadUpdate::Finished { generation, result } => {
                    finished = true;
                    received_finished = true;
                    if generation != self.browser_generation {
                        continue;
                    }
                    self.browser_loading = false;
                    match result {
                        Ok(result)
                            if result.root == self.current_dir
                                && result.view == self.browser_view =>
                        {
                            let live_preferred = self
                                .browser_user_navigated
                                .then(|| self.selected_entry().map(|entry| entry.path.clone()))
                                .flatten();
                            self.cursor = live_preferred
                                .as_ref()
                                .or(result.preferred.as_ref())
                                .or_else(|| self.selector_memory.get(&result.root))
                                .and_then(|path| {
                                    result.entries.iter().position(|entry| &entry.path == path)
                                })
                                .unwrap_or_else(|| {
                                    result
                                        .fallback_cursor
                                        .min(result.entries.len().saturating_sub(1))
                                });
                            self.browser_loaded_entries = result.entries.len();
                            self.entries = result.entries;
                            self.tree_depths = result.depths;
                            self.loaded_dir = result.root;
                            self.browser_load_elapsed = Some(result.elapsed);
                            self.browser_user_navigated = false;
                            if let Some(warning) = result.warning {
                                self.status = warning;
                            }
                        }
                        Ok(_) => {}
                        Err(error) if error != "directory load cancelled" => {
                            self.entries.clear();
                            self.tree_depths.clear();
                            self.cursor = 0;
                            self.status = error;
                        }
                        Err(_) => {}
                    }
                }
                LoadUpdate::Batch { .. } => {}
            }
        }

        if finished {
            if disconnected && !received_finished && active_generation == self.browser_generation {
                self.browser_loading = false;
                self.status = "directory load worker stopped unexpectedly".into();
            }
            self.browser_load = None;
            if let Some(request) = self.pending_browser_load.take() {
                self.browser_loaded_entries = 0;
                self.browser_loading = true;
                self.browser_load = Some(browser_loader::spawn(request));
            }
        }
        true
    }

    pub(crate) fn poll_operation(&mut self) -> bool {
        let Some(operation) = &self.operation else {
            return false;
        };
        let mut finished = None;
        let mut changed = false;
        for _ in 0..512 {
            let Ok(update) = operation.receiver.try_recv() else {
                break;
            };
            changed = true;
            match update {
                OperationUpdate::Started {
                    label,
                    total_items,
                    total_bytes,
                } => {
                    self.progress.label = label;
                    self.progress.total_items = total_items;
                    self.progress.total_bytes = total_bytes;
                }
                OperationUpdate::Progress {
                    current,
                    completed_items,
                    completed_bytes,
                } => {
                    self.progress.current = Some(current);
                    self.progress.completed_items = completed_items;
                    self.progress.completed_bytes = completed_bytes;
                }
                OperationUpdate::Finished(summary) => {
                    finished = Some(summary);
                    break;
                }
            }
        }
        let Some(summary) = finished else {
            return changed;
        };
        self.operation = None;
        let return_to_trash = self.operation_trash_manager.take();
        if let Some(preferred) = self.operation_refresh_preferred.take() {
            self.refresh_browser(Some(preferred));
        } else {
            self.refresh();
        }
        if !self.operation_search_paths.is_empty() {
            self.refresh_search_results(None);
            self.operation_search_paths.clear();
        }
        if summary.failed.is_empty() && summary.warnings.is_empty() && !summary.cancelled {
            self.set_notice(format!(
                "{} completed: {} item(s)",
                summary.label, summary.completed
            ));
            self.mode = if let Some(manager) = return_to_trash {
                self.open_trash_manager(manager)
            } else {
                self.operation_return.mode()
            };
        } else {
            self.mode = AppMode::Prompt(Prompt::Summary {
                summary,
                return_to_trash,
            });
        }
        true
    }

    pub(crate) fn poll_archive(&mut self) -> bool {
        let Some(operation) = &self.archive_operation else {
            return false;
        };
        let mut finished = None;
        let mut changed = false;
        for _ in 0..512 {
            match operation.receiver.try_recv() {
                Ok(ArchiveUpdate::Started {
                    label,
                    total_items,
                    total_bytes,
                }) => {
                    self.progress.label = label;
                    self.progress.total_items = total_items;
                    self.progress.total_bytes = total_bytes;
                    changed = true;
                }
                Ok(ArchiveUpdate::Progress {
                    current,
                    completed_items,
                    completed_bytes,
                }) => {
                    self.progress.current = Some(current);
                    self.progress.completed_items = completed_items;
                    self.progress.completed_bytes = completed_bytes;
                    changed = true;
                }
                Ok(ArchiveUpdate::Finished(result)) => {
                    finished = Some(result);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("Archive worker stopped unexpectedly".into()));
                    break;
                }
            }
        }
        let Some(result) = finished else {
            return changed;
        };
        self.archive_operation = None;
        self.progress = ProgressState::default();
        match result {
            Ok(ArchiveOutcome::Created { archive, entries }) => {
                for entry in &mut self.entries {
                    entry.selected = false;
                }
                self.refresh_browser(Some(archive));
                self.set_notice(format!("Archive created: {entries} item(s)"));
                if self.archive_return == ReturnDestination::SearchResults {
                    self.refresh_search_results(None);
                }
                self.mode = self.archive_return.mode();
            }
            Ok(ArchiveOutcome::Listed { archive, entries }) => {
                self.mode = AppMode::Archive(ArchiveView {
                    archive,
                    entries,
                    selected: 0,
                });
            }
            Ok(ArchiveOutcome::Extracted {
                archive,
                destination,
                entries,
            }) => {
                self.refresh();
                self.set_notice(format!(
                    "Extracted {} item(s) from {} to {}",
                    entries,
                    archive.display(),
                    destination.display()
                ));
                if self.archive_return == ReturnDestination::SearchResults {
                    self.refresh_search_results(None);
                }
                self.mode = self.archive_return.mode();
            }
            Err(error) if error == "Archive operation cancelled" => {
                self.set_notice("Archive operation cancelled");
                self.mode = self.archive_return.mode();
            }
            Err(error) => {
                self.modal_return = self.archive_return;
                self.mode = AppMode::Prompt(Prompt::Message {
                    title: "Archive operation failed".into(),
                    body: error,
                });
            }
        }
        true
    }

    pub(crate) fn poll_luks_operation(&mut self) -> bool {
        let retry = self.luks_operation.as_ref().and_then(|running| {
            running.retry.as_ref().map(|retry| LuksRetry {
                source: retry.source.clone(),
                label: retry.label.clone(),
                size: retry.size,
            })
        });
        let mut result = None;
        let mut changed = false;
        if let Some(running) = &self.luks_operation {
            loop {
                match running.receiver.try_recv() {
                    Ok(LuksUpdate::Phase { label, started_at }) => {
                        self.progress.phase = Some(label.into());
                        self.progress.phase_started_at = Some(started_at);
                        changed = true;
                    }
                    Ok(LuksUpdate::Finished(finished)) => {
                        result = Some(finished);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        result = Some(Err(crate::error::MinfmError::Message(
                            "encrypted-volume worker stopped unexpectedly".into(),
                        )));
                        break;
                    }
                }
            }
        }
        let Some(result) = result else {
            return changed;
        };
        let elapsed = self
            .luks_operation
            .take()
            .map(|running| running.started_at.elapsed())
            .unwrap_or_default();
        match result {
            Ok(mut outcome) => {
                outcome.message = format!("{} · took {}", outcome.message, format_elapsed(elapsed));
                self.set_notice(outcome.message);
                if let Some(pending) = self.partition_preflight.as_mut() {
                    if let Some(source) = pending.remaining.pop_front() {
                        self.start_luks(LuksAction::UnmountFilesystem { source }, None);
                        self.mode = AppMode::Progress;
                        return true;
                    }
                    let selected_path = pending
                        .view
                        .entries
                        .get(pending.view.selected)
                        .map(|entry| entry.device.path.clone());
                    self.mode = AppMode::Partitions(pending.view.clone());
                    self.start_partition_refresh(selected_path);
                } else if let Some(mountpoint) = outcome.mountpoint.filter(|path| path.is_dir()) {
                    self.mode = AppMode::Prompt(Prompt::Mounted { path: mountpoint });
                } else {
                    self.mode = self.reopen_partitions();
                }
            }
            Err(crate::error::MinfmError::IncorrectPassphrase) => {
                if let Some(retry) = retry {
                    self.mode = AppMode::Prompt(Prompt::LuksPassphrase {
                        source: retry.source,
                        label: retry.label,
                        size: retry.size,
                        input: SecretInput::default(),
                        error: Some("Incorrect passphrase. Try again.".into()),
                    });
                } else {
                    self.mode = AppMode::Prompt(Prompt::Message {
                        title: "Incorrect passphrase".into(),
                        body: "The passphrase was not accepted. The volume remains locked.".into(),
                    });
                }
            }
            Err(error) => {
                self.status = format!(
                    "Encrypted-volume operation failed after {}: {error}",
                    format_elapsed(elapsed)
                );
                if let Some(pending) = self.partition_preflight.take() {
                    self.mode = AppMode::Partitions(pending.view);
                } else {
                    self.mode = self.reopen_partitions();
                }
            }
        }
        true
    }

    pub(crate) fn poll_update(&mut self) -> bool {
        let mut changed = false;
        let check_result =
            self.update_check
                .as_ref()
                .and_then(|check| match check.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(updater::CheckOutcome::Unavailable),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(result) = check_result {
            changed = true;
            self.update_check = None;
            if let updater::CheckOutcome::Available { version } = result {
                self.pending_update = Some(version);
            }
        }

        let update_result =
            self.update
                .as_ref()
                .and_then(|update| match update.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("The update worker stopped unexpectedly".into()))
                    }
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(result) = update_result {
            changed = true;
            self.update = None;
            self.mode = match result {
                Ok(version) => AppMode::Prompt(Prompt::Message {
                    title: "Update installed".into(),
                    body: format!(
                        "minfm {version} was installed successfully.\n\nRestart minfm to use the new version."
                    ),
                }),
                Err(error) => AppMode::Prompt(Prompt::Message {
                    title: "Update failed".into(),
                    body: error,
                }),
            };
        }

        if matches!(self.mode, AppMode::Browser) {
            if let Some(latest) = self.pending_update.take() {
                changed = true;
                self.mode = AppMode::Prompt(Prompt::UpdateAvailable {
                    current: format!("v{}", env!("CARGO_PKG_VERSION")),
                    latest,
                });
            }
        }
        changed
    }

    pub(crate) fn poll_devices(&mut self) -> bool {
        let result =
            self.device_refresh
                .as_ref()
                .and_then(|refresh| match refresh.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("device refresh worker stopped unexpectedly".into()))
                    }
                    Err(TryRecvError::Empty) => None,
                });
        let mut changed = false;
        if let Some(result) = result {
            let selected_source = self
                .device_refresh
                .as_ref()
                .and_then(|refresh| refresh.selected_source.clone());
            self.device_refresh = None;
            self.device_refreshing = false;
            changed = true;
            if matches!(self.mode, AppMode::Devices(_)) {
                match result {
                    Ok(devices) => {
                        let selected = selected_source
                            .and_then(|source| {
                                devices.iter().position(|device| device.source == source)
                            })
                            .unwrap_or(0);
                        self.mode = AppMode::Devices(DeviceView { devices, selected });
                    }
                    Err(error) => self.status = format!("Automatic disk refresh failed: {error}"),
                }
            }
        }

        if self.device_refresh.is_none()
            && matches!(self.mode, AppMode::Devices(_))
            && self.last_device_refresh.elapsed() >= Duration::from_secs(1)
        {
            let selected_source = match &self.mode {
                AppMode::Devices(view) => view
                    .devices
                    .get(view.selected)
                    .map(|device| device.source.clone()),
                _ => None,
            };
            self.start_device_refresh(selected_source);
            changed = true;
        }
        changed
    }

    pub(crate) fn poll_network(&mut self) -> bool {
        let refresh_update =
            self.network_refresh
                .as_ref()
                .and_then(|refresh| match refresh.receiver.try_recv() {
                    Ok(update) => Some(update),
                    Err(TryRecvError::Disconnected) => Some(NetworkRefreshUpdate::Snapshot {
                        result: Err("network refresh worker stopped unexpectedly".into()),
                        finished: true,
                        secret_storage: None,
                    }),
                    Err(TryRecvError::Empty) => None,
                });
        let mut changed = false;
        if let Some(NetworkRefreshUpdate::Snapshot {
            result,
            finished,
            secret_storage,
        }) = refresh_update
        {
            if let Some(available) = secret_storage {
                self.network_secret_storage_available = available;
            }
            let selected_uri = match &self.mode {
                AppMode::Network(view) => view
                    .shares
                    .get(view.selected)
                    .map(|share| share.address.uri.clone()),
                _ => None,
            }
            .or_else(|| {
                self.network_refresh
                    .as_ref()
                    .and_then(|refresh| refresh.selected_uri.clone())
            });
            if finished {
                self.network_refresh = None;
                self.network_refreshing = false;
            }
            changed = true;
            if matches!(self.mode, AppMode::Network(_)) {
                match result {
                    Ok(shares) => {
                        let selected = selected_uri
                            .and_then(|uri| {
                                shares.iter().position(|share| share.address.uri == uri)
                            })
                            .unwrap_or(0);
                        self.mode = AppMode::Network(NetworkView { shares, selected });
                    }
                    Err(error) => {
                        self.mode = AppMode::Prompt(Prompt::SmbMessage {
                            title: "Network shares unavailable".into(),
                            body: error,
                            return_to_network: false,
                        });
                    }
                }
            }
        }

        let operation_result =
            self.network_operation.as_ref().and_then(|operation| {
                match operation.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("network operation worker stopped unexpectedly".into()))
                    }
                    Err(TryRecvError::Empty) => None,
                }
            });
        if let Some(result) = operation_result {
            self.network_operation = None;
            changed = true;
            match result {
                Ok(NetworkOutcome::Connected {
                    address,
                    mount_path: Some(path),
                    remembered,
                }) => {
                    if remembered {
                        self.set_notice("Share connected and remembered");
                    }
                    self.mode = AppMode::Prompt(Prompt::SmbMounted { address, path });
                }
                Ok(NetworkOutcome::Connected {
                    address,
                    mount_path: None,
                    remembered,
                }) => {
                    let remembered = if remembered {
                        " The credentials were remembered."
                    } else {
                        ""
                    };
                    self.mode = AppMode::Prompt(Prompt::SmbMessage {
                        title: "Share connected".into(),
                        body: format!(
                            "{} connected, but its local GVFS path is not available yet.{}\n\nRefresh Network Shares to open it.",
                            address.uri, remembered
                        ),
                        return_to_network: true,
                    });
                }
                Ok(NetworkOutcome::Disconnected(message) | NetworkOutcome::Forgotten(message)) => {
                    self.set_notice(message);
                    self.mode = self.open_network();
                }
                Ok(NetworkOutcome::CredentialsRequired {
                    address,
                    username,
                    domain,
                    reason,
                }) => {
                    self.mode = AppMode::Prompt(Prompt::SmbPassword {
                        address,
                        username,
                        domain,
                        input: NetworkSecret::default(),
                        error: Some(reason),
                    });
                }
                Err(error) => {
                    self.mode = AppMode::Prompt(Prompt::SmbMessage {
                        title: "Network operation failed".into(),
                        body: error,
                        return_to_network: true,
                    });
                }
            }
        }
        changed
    }

    pub(crate) fn start_device_refresh(&mut self, selected_source: Option<PathBuf>) {
        if self.device_refresh.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = luks::discover().map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.last_device_refresh = Instant::now();
        self.device_refreshing = true;
        self.device_refresh = Some(RunningDeviceRefresh {
            receiver,
            selected_source,
        });
    }

    pub(crate) fn poll_partitions(&mut self) -> bool {
        let result =
            self.partition_refresh
                .as_ref()
                .and_then(|refresh| match refresh.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("partition refresh worker stopped unexpectedly".into()))
                    }
                    Err(TryRecvError::Empty) => None,
                });
        let Some(result) = result else {
            return false;
        };
        let selected_path = self
            .partition_refresh
            .as_ref()
            .and_then(|refresh| refresh.selected_path.clone());
        self.partition_refresh = None;
        self.partition_refreshing = false;
        if matches!(self.mode, AppMode::Partitions(_)) {
            match result {
                Ok(inventory) => {
                    let selected = selected_path
                        .and_then(|path| {
                            inventory
                                .entries
                                .iter()
                                .position(|entry| entry.device.path == path)
                        })
                        .unwrap_or(0);
                    let refreshed = PartitionView {
                        entries: inventory.entries,
                        selected,
                        overlay: None,
                    };
                    if self
                        .partition_preflight
                        .as_ref()
                        .is_some_and(|pending| pending.remaining.is_empty())
                    {
                        let task = self.partition_preflight.take().map(|pending| pending.task);
                        self.mode = task
                            .map(|task| self.begin_partition_task(refreshed.clone(), task))
                            .unwrap_or(AppMode::Partitions(refreshed));
                    } else {
                        self.mode = AppMode::Partitions(refreshed);
                    }
                }
                Err(error) => {
                    self.status = format!("Partition refresh failed: {error}");
                }
            }
        }
        true
    }

    pub(crate) fn start_partition_refresh(&mut self, selected_path: Option<PathBuf>) {
        if self.partition_refresh.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = partition::discover().map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.partition_refreshing = true;
        self.partition_refresh = Some(RunningPartitionRefresh {
            receiver,
            selected_path,
        });
    }

    pub(crate) fn poll_partition_operation(&mut self) -> bool {
        let update = self.partition_operation.as_ref().and_then(|operation| {
            match operation.receiver.try_recv() {
                Ok(update) => Some(update),
                Err(TryRecvError::Disconnected) => Some(PartitionUpdate::Finished(Err(
                    crate::error::MinfmError::Message(
                        "partition operation worker stopped unexpectedly".into(),
                    ),
                ))),
                Err(TryRecvError::Empty) => None,
            }
        });
        let Some(update) = update else {
            return false;
        };
        match update {
            PartitionUpdate::Phase { label, started_at } => {
                self.progress.phase = Some(label.into());
                self.progress.phase_started_at = Some(started_at);
            }
            PartitionUpdate::Finished(result) => {
                let finished = self.partition_operation.take();
                let elapsed = finished
                    .as_ref()
                    .map(|operation| operation.started_at.elapsed())
                    .unwrap_or_default();
                let retry_action = finished.map(|operation| operation.action);
                let mut view = self.partition_return_view.take().unwrap_or(PartitionView {
                    entries: Vec::new(),
                    selected: 0,
                    overlay: None,
                });
                view.overlay = None;
                let selected_path = view
                    .entries
                    .get(view.selected)
                    .map(|entry| entry.device.path.clone());
                match result {
                    Ok(message) => {
                        if retry_action.as_ref().is_some_and(|action| {
                            matches!(
                                action,
                                PartitionAction::SmartTest { .. }
                                    | PartitionAction::SmartReport { .. }
                            )
                        }) {
                            self.mode = AppMode::Prompt(Prompt::SmartReport {
                                body: format!(
                                    "{message}\n\nCompleted in {}",
                                    format_elapsed(elapsed)
                                ),
                                scroll: 0,
                                view,
                            });
                            return true;
                        } else {
                            self.set_notice(format!("{message} in {}", format_elapsed(elapsed)));
                        }
                    }
                    Err(crate::error::MinfmError::IncorrectPassphrase) => {
                        if let Some(action) = retry_action {
                            self.mode = AppMode::Prompt(Prompt::PartitionAuthentication {
                                action,
                                view,
                                input: SecretInput::default(),
                                error: Some("Authentication failed. Try again.".into()),
                            });
                            return true;
                        }
                        self.status = "Administrator authentication failed".into();
                    }
                    Err(error) => {
                        let action = retry_action
                            .as_ref()
                            .map(PartitionAction::title)
                            .unwrap_or("Partition operation");
                        let target = retry_action
                            .as_ref()
                            .map(|action| action.target().path.display().to_string())
                            .unwrap_or_else(|| "Unknown device".into());
                        self.status = "Partition operation failed".into();
                        self.mode = AppMode::Prompt(Prompt::PartitionError {
                            body: format!("{action}\n{target}\n\n{error}\n\nCheck the device before retrying."),
                            view,
                        });
                        return true;
                    }
                }
                self.mode = AppMode::Partitions(view);
                self.start_partition_refresh(selected_path);
            }
        }
        true
    }

    pub(crate) fn start_partition_operation(
        &mut self,
        action: PartitionAction,
        mut view: PartitionView,
        password: Option<SecretInput>,
    ) {
        let title = action.title();
        let current = action.target().path.clone();
        let (sender, receiver) = mpsc::sync_channel(8);
        let started_at = Instant::now();
        let retry_action = action.clone();
        thread::spawn(move || {
            let result = partition::execute(
                &action,
                password.as_ref().map(SecretInput::expose),
                |label| {
                    let _ = sender.send(PartitionUpdate::Phase {
                        label,
                        started_at: Instant::now(),
                    });
                },
            );
            let _ = sender.send(PartitionUpdate::Finished(result));
        });
        view.overlay = None;
        self.partition_return_view = Some(view);
        self.progress = ProgressState {
            label: title.into(),
            phase: Some("Preparing partition operation".into()),
            current: Some(current),
            total_items: 0,
            completed_items: 0,
            total_bytes: 0,
            completed_bytes: 0,
            cancelling: false,
            cancellable: false,
            started_at: Some(started_at),
            phase_started_at: Some(started_at),
        };
        self.partition_operation = Some(RunningPartitionOperation {
            receiver,
            started_at,
            action: retry_action,
        });
    }

    pub(crate) fn poll_status_expiry(&mut self) -> bool {
        let Some((message, deadline)) = &self.status_expiry else {
            return false;
        };
        if Instant::now() < *deadline {
            return false;
        }
        if &self.status == message {
            self.status.clear();
        }
        self.status_expiry = None;
        true
    }

    pub(crate) fn needs_animation(&self) -> bool {
        self.search.is_some()
            || self.browser_loading
            || self.partition_refreshing
            || matches!(self.mode, AppMode::UpdateProgress)
            || matches!(self.mode, AppMode::NetworkProgress)
            || (matches!(self.mode, AppMode::Progress)
                && self.progress.total_items == 0
                && self.progress.total_bytes == 0)
    }

    pub(crate) fn search_running(&self) -> bool {
        self.search.is_some()
    }

    pub(crate) fn poll_file_launch(&mut self) -> bool {
        while let Ok(error) = self.launch_receiver.try_recv() {
            self.pending_launch_errors.push_back(error);
        }
        let config_error = match &self.mode {
            AppMode::Browser | AppMode::SearchResults => Some(None),
            AppMode::ConfigError { path, error } => Some(Some((path.clone(), error.clone()))),
            _ => None,
        };
        let Some(config_error) = config_error else {
            return false;
        };
        let Some(pending) = self.pending_launch_errors.pop_front() else {
            return false;
        };
        self.mode = AppMode::Prompt(Prompt::OpenError {
            body: pending.error.to_string(),
            config_error,
        });
        self.modal_return = pending.return_to;
        true
    }

    pub(crate) fn take_terminal_editor(&mut self) -> Option<PendingTerminalEditor> {
        self.pending_terminal_editor.take()
    }

    pub(crate) fn finish_terminal_editor(
        &mut self,
        action: &PendingTerminalEditor,
        result: Result<(), LaunchError>,
    ) {
        match result {
            Ok(()) => {
                if action.return_to == ReturnDestination::SearchResults {
                    self.refresh_search_results(None);
                    self.mode = AppMode::SearchResults;
                    return;
                }
                let Some(snapshot) = &action.browser else {
                    return;
                };
                for entry in &mut self.entries {
                    entry.selected = snapshot.selected_paths.contains(&entry.path);
                }
                self.refresh_browser(Some(snapshot.cursor_path.clone()));
            }
            Err(error) => {
                let config_error = match &self.mode {
                    AppMode::ConfigError { path, error } => Some((path.clone(), error.clone())),
                    _ => None,
                };
                self.mode = AppMode::Prompt(Prompt::OpenError {
                    body: error.to_string(),
                    config_error,
                });
                self.modal_return = action.return_to;
            }
        }
    }
}
