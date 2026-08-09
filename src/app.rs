use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    browser_loader::{self, LoadRequest, LoadUpdate, RunningLoad},
    config::{self, Config, ConfigLoad, SortSetting},
    entry::{self, EntryKind, FileEntry},
    launcher::{self, LaunchError},
    luks::{self, LuksAction, LuksDevice, LuksOutcome, SecretInput},
    operation::{self, OperationRequest, OperationSummary, OperationUpdate, RunningOperation},
    trash::{TrashEntry, TrashManager},
    updater,
};

#[derive(Debug, Clone)]
pub enum ClipboardMode {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct Clipboard {
    pub mode: ClipboardMode,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Prompt {
    GoTo {
        input: String,
    },
    Search {
        input: String,
        scope: SearchScope,
    },
    Rename {
        source: PathBuf,
        input: String,
        cursor: usize,
    },
    CreateDirectory {
        input: String,
    },
    CreateFile {
        input: String,
        cursor: usize,
    },
    ConfirmTrash {
        paths: Vec<PathBuf>,
    },
    ConfirmOverwrite {
        sources: Vec<PathBuf>,
        cut: bool,
    },
    ConfirmRestore {
        entries: Vec<TrashEntry>,
        manager: TrashManager,
    },
    ConfirmPermanentDelete {
        entries: Vec<TrashEntry>,
        manager: TrashManager,
        clear_all: bool,
        total_bytes: u64,
    },
    ConfirmLuks {
        action: LuksAction,
        title: String,
        body: String,
    },
    LuksPassphrase {
        source: PathBuf,
        label: Option<String>,
        size: u64,
        input: SecretInput,
        error: Option<String>,
    },
    Mounted {
        path: PathBuf,
    },
    UpdateAvailable {
        current: String,
        latest: String,
    },
    Message {
        title: String,
        body: String,
    },
    OpenError {
        body: String,
        config_error: Option<(PathBuf, String)>,
    },
    Summary {
        summary: OperationSummary,
        return_to_trash: Option<TrashManager>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    CurrentDirectory,
    Filesystem,
}

#[derive(Debug, Clone)]
pub struct SearchView {
    pub query: String,
    pub results: Vec<PathBuf>,
    pub selected: usize,
    pub skipped: usize,
    pub limited: bool,
}

#[derive(Debug)]
enum SearchUpdate {
    Match(PathBuf),
    PermissionDenied,
    Finished { cancelled: bool, limited: bool },
}

struct RunningSearch {
    receiver: Receiver<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    query: String,
    results: Vec<PathBuf>,
    skipped: usize,
}

struct RunningUpdateCheck {
    receiver: Receiver<updater::CheckOutcome>,
}

struct RunningUpdate {
    receiver: Receiver<Result<String, String>>,
}

struct RunningDeviceRefresh {
    receiver: Receiver<Result<Vec<LuksDevice>, String>>,
    selected_source: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct TrashView {
    pub manager: TrashManager,
    pub entries: Vec<TrashEntry>,
    pub selected: usize,
    pub marked: HashSet<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DeviceView {
    pub devices: Vec<LuksDevice>,
    pub selected: usize,
}

pub enum AppMode {
    Browser,
    Prompt(Prompt),
    Progress,
    SearchProgress,
    SearchResults(SearchView),
    UpdateProgress,
    Trash(TrashView),
    Devices(DeviceView),
    Help,
    Info,
    ConfigError { path: PathBuf, error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserView {
    Tree,
    Table,
}

#[derive(Debug, Clone, Default)]
pub struct ProgressState {
    pub label: String,
    pub phase: Option<String>,
    pub current: Option<PathBuf>,
    pub total_items: usize,
    pub completed_items: usize,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub cancelling: bool,
    pub cancellable: bool,
    pub started_at: Option<Instant>,
    pub phase_started_at: Option<Instant>,
}

enum LuksUpdate {
    Phase {
        label: &'static str,
        started_at: Instant,
    },
    Finished(crate::error::Result<LuksOutcome>),
}

struct RunningLuks {
    receiver: Receiver<LuksUpdate>,
    retry: Option<LuksRetry>,
    started_at: Instant,
}

struct LuksRetry {
    source: PathBuf,
    label: Option<String>,
    size: u64,
}

#[derive(Debug)]
struct BrowserSnapshot {
    cursor_path: PathBuf,
    selected_paths: HashSet<PathBuf>,
}

#[derive(Debug)]
pub struct PendingTerminalEditor {
    program: String,
    path: PathBuf,
    browser: Option<BrowserSnapshot>,
}

impl PendingTerminalEditor {
    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

const STATUS_NOTICE_DURATION: Duration = Duration::from_secs(10);

pub struct App {
    pub running: bool,
    pub current_dir: PathBuf,
    pub browser_view: BrowserView,
    pub entries: Vec<FileEntry>,
    pub tree_depths: Vec<usize>,
    pub cursor: usize,
    pub config: Config,
    pub config_path: PathBuf,
    pub mode: AppMode,
    pub clipboard: Option<Clipboard>,
    pub status: String,
    status_expiry: Option<(String, Instant)>,
    pub progress: ProgressState,
    operation: Option<RunningOperation>,
    operation_trash_manager: Option<TrashManager>,
    luks_operation: Option<RunningLuks>,
    launch_sender: SyncSender<LaunchError>,
    launch_receiver: Receiver<LaunchError>,
    pending_launch_errors: VecDeque<LaunchError>,
    pending_terminal_editor: Option<PendingTerminalEditor>,
    last_device_refresh: Instant,
    device_refresh: Option<RunningDeviceRefresh>,
    pub device_refreshing: bool,
    selector_memory: HashMap<PathBuf, PathBuf>,
    expanded_directories: HashSet<PathBuf>,
    loaded_dir: PathBuf,
    browser_load: Option<RunningLoad>,
    pending_browser_load: Option<LoadRequest>,
    browser_generation: u64,
    pub browser_loading: bool,
    pub browser_loaded_entries: usize,
    pub browser_load_elapsed: Option<Duration>,
    browser_user_navigated: bool,
    pending_directory_search: Option<String>,
    pub search_filter: Option<String>,
    search: Option<RunningSearch>,
    pub search_matches: usize,
    pub search_skipped: usize,
    pub search_cancelling: bool,
    update_check: Option<RunningUpdateCheck>,
    update: Option<RunningUpdate>,
    pending_update: Option<String>,
}

impl App {
    pub fn new(start: PathBuf, load: ConfigLoad, force_read_only: bool) -> Self {
        let (mut config, config_path, mode) = match load {
            ConfigLoad::Valid { config, path } => (config, path, AppMode::Browser),
            ConfigLoad::Invalid { path, error } => (
                Config::default(),
                path.clone(),
                AppMode::ConfigError { path, error },
            ),
        };
        config.behavior.read_only |= force_read_only;
        let (launch_sender, launch_receiver) = mpsc::sync_channel(16);
        let mut app = Self {
            running: true,
            current_dir: start.clone(),
            browser_view: BrowserView::Tree,
            entries: Vec::new(),
            tree_depths: Vec::new(),
            cursor: 0,
            config,
            config_path,
            mode,
            clipboard: None,
            status: String::new(),
            status_expiry: None,
            progress: ProgressState::default(),
            operation: None,
            operation_trash_manager: None,
            luks_operation: None,
            launch_sender,
            launch_receiver,
            pending_launch_errors: VecDeque::new(),
            pending_terminal_editor: None,
            last_device_refresh: Instant::now(),
            device_refresh: None,
            device_refreshing: false,
            selector_memory: HashMap::new(),
            expanded_directories: HashSet::new(),
            loaded_dir: start.clone(),
            browser_load: None,
            pending_browser_load: None,
            browser_generation: 0,
            browser_loading: false,
            browser_loaded_entries: 0,
            browser_load_elapsed: None,
            browser_user_navigated: false,
            pending_directory_search: None,
            search_filter: None,
            search: None,
            search_matches: 0,
            search_skipped: 0,
            search_cancelling: false,
            update_check: None,
            update: None,
            pending_update: None,
        };
        if matches!(app.mode, AppMode::Browser) {
            app.refresh();
            if cfg!(not(test)) {
                app.start_update_check();
            }
        }
        app
    }

    pub fn visible_status(&self) -> &str {
        match &self.status_expiry {
            Some((message, deadline)) if message == &self.status && Instant::now() >= *deadline => {
                ""
            }
            _ => &self.status,
        }
    }

    pub fn browser_view_label(&self) -> &'static str {
        match self.browser_view {
            BrowserView::Tree => "Tree",
            BrowserView::Table => "Table",
        }
    }

    pub fn tree_depth(&self, index: usize) -> usize {
        self.tree_depths.get(index).copied().unwrap_or(0)
    }

    pub fn is_tree_directory_expanded(&self, path: &Path) -> bool {
        self.expanded_directories.contains(path)
    }

    fn set_notice(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_expiry = Some((self.status.clone(), Instant::now() + STATUS_NOTICE_DURATION));
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.kind == KeyEventKind::Repeat
            && matches!(self.mode, AppMode::Browser)
            && !matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k')
            )
        {
            return;
        }
        // This match is the modal-isolation boundary. Browser shortcuts are never
        // considered while any prompt, popup, error, or progress mode owns focus.
        let mode = std::mem::replace(&mut self.mode, AppMode::Browser);
        self.mode = match mode {
            AppMode::Browser => self.handle_browser_key(key),
            AppMode::Prompt(prompt) => self.handle_prompt_key(prompt, key),
            AppMode::Progress => self.handle_progress_key(key),
            AppMode::SearchProgress => self.handle_search_progress_key(key),
            AppMode::SearchResults(view) => self.handle_search_results_key(view, key),
            AppMode::UpdateProgress => AppMode::UpdateProgress,
            AppMode::Trash(view) => self.handle_trash_key(view, key),
            AppMode::Devices(view) => self.handle_device_key(view, key),
            AppMode::Help => self.handle_readonly_popup(key, AppMode::Help),
            AppMode::Info => self.handle_readonly_popup(key, AppMode::Info),
            AppMode::ConfigError { path, error } => self.handle_config_error(path, error, key),
        };
    }

    pub fn poll_browser_load(&mut self) -> bool {
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
        let mut reload_without_filter = false;
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
                            let entry_count = result.entries.len();
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
                            if let Some(query) = self.pending_directory_search.take() {
                                if entry_count == 0 {
                                    self.search_filter = None;
                                    self.status = format!("No match for {query}");
                                    reload_without_filter = true;
                                } else {
                                    self.set_notice(format!("Search: {entry_count} match(es)"));
                                }
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
            if reload_without_filter {
                self.pending_browser_load = None;
                self.refresh();
            } else if let Some(request) = self.pending_browser_load.take() {
                self.browser_loaded_entries = 0;
                self.browser_loading = true;
                self.browser_load = Some(browser_loader::spawn(request));
            }
        }
        true
    }

    pub fn poll_operation(&mut self) -> bool {
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
        self.refresh();
        if summary.failed.is_empty() && summary.warnings.is_empty() && !summary.cancelled {
            self.set_notice(format!(
                "{} completed: {} item(s)",
                summary.label, summary.completed
            ));
            self.mode = if let Some(manager) = return_to_trash {
                self.open_trash_manager(manager)
            } else {
                AppMode::Browser
            };
        } else {
            self.mode = AppMode::Prompt(Prompt::Summary {
                summary,
                return_to_trash,
            });
        }
        true
    }

    pub fn poll_luks_operation(&mut self) -> bool {
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
                if let Some(mountpoint) = outcome.mountpoint.filter(|path| path.is_dir()) {
                    self.mode = AppMode::Prompt(Prompt::Mounted { path: mountpoint });
                } else {
                    self.mode = self.open_devices();
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
                self.mode = self.open_devices();
            }
        }
        true
    }

    pub fn poll_search(&mut self) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        let mut finished = None;
        let mut changed = false;
        for _ in 0..1_024 {
            let Ok(update) = search.receiver.try_recv() else {
                break;
            };
            changed = true;
            match update {
                SearchUpdate::Match(path) => {
                    search.results.push(path);
                    self.search_matches = search.results.len();
                }
                SearchUpdate::PermissionDenied => {
                    search.skipped += 1;
                    self.search_skipped = search.skipped;
                }
                SearchUpdate::Finished { cancelled, limited } => {
                    finished = Some((cancelled, limited))
                }
            }
        }
        let Some((cancelled, limited)) = finished else {
            return changed;
        };
        let Some(search) = self.search.take() else {
            return changed;
        };
        self.search_cancelling = false;
        if cancelled {
            self.mode = AppMode::Browser;
            self.set_notice("Filesystem search cancelled");
        } else {
            self.mode = AppMode::SearchResults(SearchView {
                query: search.query,
                results: search.results,
                selected: 0,
                skipped: search.skipped,
                limited,
            });
            if self.search_matches == 0 {
                self.set_notice("No filesystem matches found");
            }
        }
        true
    }

    pub fn poll_update(&mut self) -> bool {
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

    pub fn poll_devices(&mut self) -> bool {
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

    fn start_device_refresh(&mut self, selected_source: Option<PathBuf>) {
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

    pub fn poll_status_expiry(&mut self) -> bool {
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

    pub fn needs_animation(&self) -> bool {
        self.browser_loading
            || matches!(self.mode, AppMode::UpdateProgress)
            || (matches!(self.mode, AppMode::Progress)
                && self.progress.total_items == 0
                && self.progress.total_bytes == 0)
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.cursor)
    }

    pub fn poll_file_launch(&mut self) -> bool {
        while let Ok(error) = self.launch_receiver.try_recv() {
            self.pending_launch_errors.push_back(error);
        }
        let config_error = match &self.mode {
            AppMode::Browser => Some(None),
            AppMode::ConfigError { path, error } => Some(Some((path.clone(), error.clone()))),
            _ => None,
        };
        let Some(config_error) = config_error else {
            return false;
        };
        let Some(error) = self.pending_launch_errors.pop_front() else {
            return false;
        };
        self.mode = AppMode::Prompt(Prompt::OpenError {
            body: error.to_string(),
            config_error,
        });
        true
    }

    pub fn take_terminal_editor(&mut self) -> Option<PendingTerminalEditor> {
        self.pending_terminal_editor.take()
    }

    pub fn finish_terminal_editor(
        &mut self,
        action: &PendingTerminalEditor,
        result: Result<(), LaunchError>,
    ) {
        match result {
            Ok(()) => {
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
            }
        }
    }

    pub fn sort_label(&self) -> &'static str {
        match self.config.ui.sort {
            SortSetting::Name => "name",
            SortSetting::Extension => "extension",
            SortSetting::Size => "size",
            SortSetting::Modified => "modified",
            SortSetting::Type => "type",
            SortSetting::Permissions => "permissions",
        }
    }

    fn handle_browser_key(&mut self, key: KeyEvent) -> AppMode {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.running = false;
            return AppMode::Browser;
        }
        match key.code {
            KeyCode::Esc if self.search_filter.is_some() => {
                self.search_filter = None;
                self.refresh();
                self.set_notice("Search cleared");
            }
            KeyCode::Char('q') => self.running = false,
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Enter => {
                return match self.browser_view {
                    BrowserView::Tree => self.activate_tree_entry(),
                    BrowserView::Table => self.open_selected_table(),
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                return match self.browser_view {
                    BrowserView::Tree => self.tree_right(),
                    BrowserView::Table => self.open_selected_table(),
                }
            }
            KeyCode::Left | KeyCode::Char('h') => match self.browser_view {
                BrowserView::Tree => self.tree_left(),
                BrowserView::Table => self.go_parent(),
            },
            KeyCode::Char('v') => self.toggle_browser_view(),
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('.') => {
                self.config.ui.show_hidden = !self.config.ui.show_hidden;
                self.refresh();
            }
            KeyCode::Char('s') => {
                self.cycle_sort();
                self.refresh();
            }
            KeyCode::Char('S') => {
                self.config.ui.reverse_sort = !self.config.ui.reverse_sort;
                self.refresh();
            }
            KeyCode::Char('g') => {
                return AppMode::Prompt(Prompt::GoTo {
                    input: String::new(),
                })
            }
            KeyCode::Char('/') => {
                return AppMode::Prompt(Prompt::Search {
                    input: String::new(),
                    scope: SearchScope::CurrentDirectory,
                })
            }
            KeyCode::Char('F') => {
                return AppMode::Prompt(Prompt::Search {
                    input: String::new(),
                    scope: SearchScope::Filesystem,
                })
            }
            KeyCode::Char('r') => {
                if let Some(entry) = self.selected_entry() {
                    let input = entry.name.clone();
                    return AppMode::Prompt(Prompt::Rename {
                        source: entry.path.clone(),
                        cursor: input.chars().count(),
                        input,
                    });
                }
            }
            KeyCode::Char('a') => {
                return AppMode::Prompt(Prompt::CreateDirectory {
                    input: String::new(),
                })
            }
            KeyCode::Char('n') => {
                return AppMode::Prompt(Prompt::CreateFile {
                    input: String::new(),
                    cursor: 0,
                })
            }
            KeyCode::Char('c') => self.set_clipboard(ClipboardMode::Copy),
            KeyCode::Char('x') => self.set_clipboard(ClipboardMode::Cut),
            KeyCode::Char('p') => return self.prepare_paste(),
            KeyCode::Char('d') => {
                if let Some(paths) = self.mutation_targets() {
                    return AppMode::Prompt(Prompt::ConfirmTrash { paths });
                }
            }
            KeyCode::Char('D') => {
                if let Some(paths) = self.mutation_targets() {
                    self.start_trash(paths);
                    return AppMode::Progress;
                }
            }
            KeyCode::Char('T') => return self.open_trash(),
            KeyCode::Char('I') => return AppMode::Info,
            KeyCode::Char('?') => return AppMode::Help,
            KeyCode::Char('o') | KeyCode::Char('e') => {
                return self.open_external(key.code == KeyCode::Char('e'))
            }
            KeyCode::Char('m') => {
                if self.config.behavior.read_only {
                    self.status = "Read-only mode: disk operations are disabled".into();
                } else {
                    return self.open_devices();
                }
            }
            _ => {}
        }
        AppMode::Browser
    }

    fn handle_prompt_key(&mut self, mut prompt: Prompt, key: KeyEvent) -> AppMode {
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
            Prompt::Search { input, scope } => {
                if edit_input(input, key) {
                    return AppMode::Browser;
                }
                if key.code == KeyCode::Enter {
                    let query = input.trim().to_string();
                    if query.is_empty() {
                        self.status = "Search query cannot be empty".into();
                        return AppMode::Browser;
                    }
                    return match scope {
                        SearchScope::CurrentDirectory => {
                            self.search_here(&query);
                            AppMode::Browser
                        }
                        SearchScope::Filesystem => {
                            self.start_filesystem_search(&query);
                            AppMode::SearchProgress
                        }
                    };
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
            Prompt::Rename {
                source,
                input,
                cursor,
            } => {
                if edit_cursor_input(input, cursor, key) {
                    return AppMode::Browser;
                }
                if key.code == KeyCode::Enter {
                    let source = source.clone();
                    let input = input.clone();
                    self.rename(&source, &input);
                    return AppMode::Browser;
                }
            }
            Prompt::ConfirmTrash { paths } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.start_trash(paths.clone());
                    return AppMode::Progress;
                }
                KeyCode::Esc | KeyCode::Char('n') => return AppMode::Browser,
                _ => {}
            },
            Prompt::ConfirmOverwrite { sources, cut } => match key.code {
                KeyCode::Char('o') | KeyCode::Enter => {
                    self.start_copy(sources.clone(), *cut, true);
                    return AppMode::Progress;
                }
                KeyCode::Char('s') => {
                    let filtered = sources
                        .iter()
                        .filter(|source| {
                            source
                                .file_name()
                                .map(|name| !self.current_dir.join(name).exists())
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if filtered.is_empty() {
                        self.set_notice("All conflicting items were skipped");
                        return AppMode::Browser;
                    }
                    self.start_copy(filtered, *cut, false);
                    return AppMode::Progress;
                }
                KeyCode::Esc | KeyCode::Char('a') => return AppMode::Browser,
                _ => {}
            },
            Prompt::ConfirmRestore { entries, manager } => match key.code {
                KeyCode::Enter | KeyCode::Char('r') => {
                    let mut restored = 0;
                    let mut failures = Vec::new();
                    for entry in entries.iter() {
                        match manager.restore(entry, None) {
                            Ok(_) => restored += 1,
                            Err(error) => {
                                failures.push((entry.trashed_path.clone(), error.to_string()))
                            }
                        }
                    }
                    if failures.is_empty() {
                        self.set_notice(format!("Restored {restored} item(s)"));
                        self.refresh();
                        return self.open_trash_manager(manager.clone());
                    }
                    return AppMode::Prompt(Prompt::Summary {
                        summary: OperationSummary {
                            label: "Restoring".into(),
                            completed: restored,
                            failed: failures,
                            ..OperationSummary::default()
                        },
                        return_to_trash: Some(manager.clone()),
                    });
                }
                KeyCode::Esc => return self.open_trash(),
                _ => {}
            },
            Prompt::ConfirmPermanentDelete {
                entries, manager, ..
            } => match key.code {
                KeyCode::Enter | KeyCode::Char('d') => {
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
                KeyCode::Esc => return self.open_devices(),
                _ => {}
            },
            Prompt::LuksPassphrase {
                source,
                label,
                size,
                input,
                error,
            } => match key.code {
                KeyCode::Esc => return self.open_devices(),
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
            Prompt::Mounted { path } => match key.code {
                KeyCode::Enter => {
                    let path = path.clone();
                    self.go_to(path);
                    return AppMode::Browser;
                }
                KeyCode::Esc => return AppMode::Browser,
                _ => {}
            },
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
                    return AppMode::Browser;
                }
            }
            Prompt::OpenError { config_error, .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return if let Some((path, error)) = config_error.take() {
                        AppMode::ConfigError { path, error }
                    } else {
                        AppMode::Browser
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
                        AppMode::Browser
                    };
                }
            }
        }
        AppMode::Prompt(prompt)
    }

    fn handle_progress_key(&mut self, key: KeyEvent) -> AppMode {
        if key.code == KeyCode::Esc && self.progress.cancellable {
            if let Some(operation) = &self.operation {
                operation.cancel.store(true, Ordering::Relaxed);
                self.progress.cancelling = true;
            }
        }
        AppMode::Progress
    }

    fn handle_search_progress_key(&mut self, key: KeyEvent) -> AppMode {
        if key.code == KeyCode::Esc {
            if let Some(search) = &self.search {
                search.cancel.store(true, Ordering::Relaxed);
                self.search_cancelling = true;
            }
        }
        AppMode::SearchProgress
    }

    fn handle_search_results_key(&mut self, view: SearchView, key: KeyEvent) -> AppMode {
        let mut view = view;
        match key.code {
            KeyCode::Esc => AppMode::Browser,
            KeyCode::Down | KeyCode::Char('j') => {
                if !view.results.is_empty() {
                    view.selected = (view.selected + 1).min(view.results.len() - 1);
                }
                AppMode::SearchResults(view)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::SearchResults(view)
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(path) = view.results.get(view.selected).cloned() {
                    self.open_search_result(&path);
                    AppMode::Browser
                } else {
                    AppMode::SearchResults(view)
                }
            }
            KeyCode::Char('/') => AppMode::Prompt(Prompt::Search {
                input: String::new(),
                scope: SearchScope::CurrentDirectory,
            }),
            KeyCode::Char('F') => AppMode::Prompt(Prompt::Search {
                input: String::new(),
                scope: SearchScope::Filesystem,
            }),
            _ => AppMode::SearchResults(view),
        }
    }

    fn handle_trash_key(&mut self, mut view: TrashView, key: KeyEvent) -> AppMode {
        match key.code {
            KeyCode::Esc | KeyCode::Char('T') => {
                self.refresh();
                AppMode::Browser
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !view.entries.is_empty() {
                    view.selected = (view.selected + 1).min(view.entries.len() - 1);
                }
                AppMode::Trash(view)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Trash(view)
            }
            KeyCode::Char(' ') => {
                if let Some(entry) = view.entries.get(view.selected) {
                    if !view.marked.remove(&entry.trashed_path) {
                        view.marked.insert(entry.trashed_path.clone());
                    }
                }
                AppMode::Trash(view)
            }
            KeyCode::Char('r') | KeyCode::Enter => {
                let entries = trash_targets(&view);
                if entries.is_empty() {
                    AppMode::Trash(view)
                } else {
                    AppMode::Prompt(Prompt::ConfirmRestore {
                        entries,
                        manager: view.manager,
                    })
                }
            }
            KeyCode::Char('d') => {
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
            }
            KeyCode::Char('D') => {
                let entries = trash_targets(&view);
                if entries.is_empty() {
                    AppMode::Trash(view)
                } else {
                    self.start_permanent_delete(entries, view.manager);
                    AppMode::Progress
                }
            }
            KeyCode::Char('C') => {
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
            }
            _ => AppMode::Trash(view),
        }
    }

    fn handle_device_key(&mut self, mut view: DeviceView, key: KeyEvent) -> AppMode {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => AppMode::Browser,
            KeyCode::Down | KeyCode::Char('j') => {
                if !view.devices.is_empty() {
                    view.selected = (view.selected + 1).min(view.devices.len() - 1);
                }
                AppMode::Devices(view)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Devices(view)
            }
            KeyCode::Char('r') => {
                let selected_source = view
                    .devices
                    .get(view.selected)
                    .map(|device| device.source.clone());
                self.start_device_refresh(selected_source);
                AppMode::Devices(view)
            }
            KeyCode::Char('e') => {
                let Some(device) = view.devices.get(view.selected) else {
                    return AppMode::Devices(view);
                };
                if device.system_protected || !device.ejectable || device.eject_blocked {
                    return AppMode::Devices(view);
                }
                let steps = if device.is_mounted() {
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
            KeyCode::Enter | KeyCode::Char('m') | KeyCode::Char('u') => {
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
            _ => AppMode::Devices(view),
        }
    }

    fn handle_readonly_popup(&mut self, key: KeyEvent, mode: AppMode) -> AppMode {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            AppMode::Browser
        } else {
            mode
        }
    }

    fn handle_config_error(&mut self, path: PathBuf, error: String, key: KeyEvent) -> AppMode {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.running = false;
                AppMode::ConfigError { path, error }
            }
            KeyCode::Char('r') => match config::load_from(path.clone()) {
                ConfigLoad::Valid { config, path } => {
                    self.config = config;
                    self.config_path = path;
                    self.refresh();
                    self.set_notice("Configuration reloaded");
                    AppMode::Browser
                }
                ConfigLoad::Invalid { path, error } => AppMode::ConfigError { path, error },
            },
            KeyCode::Char('e') => {
                let program = launcher::resolve_editor(&self.config.open.editor);
                if launcher::is_terminal_editor(&program) {
                    self.pending_terminal_editor = Some(PendingTerminalEditor {
                        program,
                        path: path.clone(),
                        browser: None,
                    });
                    return AppMode::ConfigError { path, error };
                }
                match launcher::launch(program, path.clone(), self.launch_sender.clone()) {
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

    fn refresh(&mut self) {
        let same_root = self.loaded_dir == self.current_dir;
        let preferred = if same_root {
            self.selected_entry().map(|entry| entry.path.clone())
        } else {
            self.selector_memory.get(&self.current_dir).cloned()
        };
        if same_root {
            self.remember_selection();
        }
        self.refresh_browser(preferred);
    }

    fn refresh_browser(&mut self, preferred: Option<PathBuf>) {
        if cfg!(test) {
            match self.browser_view {
                BrowserView::Tree => self.refresh_tree(preferred),
                BrowserView::Table => self.refresh_table(preferred),
            }
        } else {
            self.request_browser_load(preferred);
        }
    }

    fn request_browser_load(&mut self, preferred: Option<PathBuf>) {
        self.browser_generation = self.browser_generation.wrapping_add(1);
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect();
        let request = LoadRequest {
            generation: self.browser_generation,
            root: self.current_dir.clone(),
            view: self.browser_view,
            ui: self.config.ui.clone(),
            expanded: self.expanded_directories.clone(),
            query: self.search_filter.clone(),
            marked,
            preferred,
            fallback_cursor: self.cursor,
        };
        self.entries.clear();
        self.tree_depths.clear();
        self.cursor = 0;
        self.browser_loading = true;
        self.browser_loaded_entries = 0;
        self.browser_load_elapsed = None;
        self.browser_user_navigated = false;
        if let Some(running) = &self.browser_load {
            running.cancel.store(true, Ordering::Relaxed);
            self.pending_browser_load = Some(request);
        } else {
            self.browser_load = Some(browser_loader::spawn(request));
        }
    }

    fn refresh_table(&mut self, preferred: Option<PathBuf>) {
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        match entry::read_directory(
            &self.current_dir,
            self.config.ui.show_hidden,
            self.config.ui.sort,
            self.config.ui.reverse_sort,
            self.config.ui.directories_first,
        ) {
            Ok(entries) => {
                let query = self.search_filter.as_deref();
                let entries = entries
                    .into_iter()
                    .filter(|entry| {
                        query.is_none_or(|query| {
                            entry::contains_case_insensitive(&entry.name, query)
                        })
                    })
                    .map(|mut entry| {
                        entry.selected = marked.contains(&entry.path);
                        entry
                    })
                    .collect::<Vec<_>>();
                self.cursor = preferred
                    .as_ref()
                    .or_else(|| self.selector_memory.get(&self.current_dir))
                    .and_then(|path| entries.iter().position(|entry| &entry.path == path))
                    .unwrap_or_else(|| self.cursor.min(entries.len().saturating_sub(1)));
                self.entries = entries;
                self.tree_depths.clear();
                self.loaded_dir = self.current_dir.clone();
            }
            Err(error) => {
                self.entries.clear();
                self.tree_depths.clear();
                self.cursor = 0;
                self.status = error.to_string();
            }
        }
    }

    fn refresh_tree(&mut self, preferred: Option<PathBuf>) {
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        match self.read_expanded_tree() {
            Ok((entries, depths, nested_error)) => {
                let query = self.search_filter.as_deref();
                let (entries, depths): (Vec<_>, Vec<_>) = entries
                    .into_iter()
                    .zip(depths)
                    .filter(|(entry, _)| {
                        query.is_none_or(|query| {
                            entry::contains_case_insensitive(&entry.name, query)
                        })
                    })
                    .map(|(mut entry, depth)| {
                        entry.selected = marked.contains(&entry.path);
                        (entry, if query.is_some() { 0 } else { depth })
                    })
                    .unzip();
                self.cursor = preferred
                    .as_ref()
                    .or_else(|| self.selector_memory.get(&self.current_dir))
                    .and_then(|path| entries.iter().position(|entry| &entry.path == path))
                    .unwrap_or_else(|| self.cursor.min(entries.len().saturating_sub(1)));
                self.entries = entries;
                self.tree_depths = depths;
                self.loaded_dir = self.current_dir.clone();
                if let Some(error) = nested_error {
                    self.status = error;
                }
            }
            Err(error) => {
                self.entries.clear();
                self.tree_depths.clear();
                self.cursor = 0;
                self.status = error.to_string();
            }
        }
    }

    fn read_expanded_tree(
        &self,
    ) -> crate::error::Result<(Vec<FileEntry>, Vec<usize>, Option<String>)> {
        fn append_directory(
            path: &Path,
            depth: usize,
            config: &Config,
            expanded: &HashSet<PathBuf>,
            entries: &mut Vec<FileEntry>,
            depths: &mut Vec<usize>,
            nested_error: &mut Option<String>,
        ) -> crate::error::Result<()> {
            let children = entry::read_directory(
                path,
                config.ui.show_hidden,
                config.ui.sort,
                config.ui.reverse_sort,
                config.ui.directories_first,
            )?;
            for child in children {
                let recurse = child.kind == EntryKind::Directory && expanded.contains(&child.path);
                let child_path = child.path.clone();
                entries.push(child);
                depths.push(depth);
                if recurse {
                    if let Err(error) = append_directory(
                        &child_path,
                        depth + 1,
                        config,
                        expanded,
                        entries,
                        depths,
                        nested_error,
                    ) {
                        nested_error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
            Ok(())
        }

        let mut entries = Vec::new();
        let mut depths = Vec::new();
        let mut nested_error = None;
        append_directory(
            &self.current_dir,
            0,
            &self.config,
            &self.expanded_directories,
            &mut entries,
            &mut depths,
            &mut nested_error,
        )?;
        Ok((entries, depths, nested_error))
    }

    fn toggle_browser_view(&mut self) {
        let selected = self.selected_entry().map(|entry| entry.path.clone());
        self.remember_selection();
        match self.browser_view {
            BrowserView::Tree => {
                if let Some(parent) = selected.as_deref().and_then(Path::parent) {
                    self.current_dir = parent.to_path_buf();
                }
                self.browser_view = BrowserView::Table;
                self.expanded_directories.clear();
                self.cursor = 0;
                self.refresh_browser(selected);
                self.set_notice("Table view");
            }
            BrowserView::Table => {
                self.browser_view = BrowserView::Tree;
                self.expanded_directories.clear();
                self.cursor = 0;
                self.refresh_browser(selected);
                self.set_notice("Tree view");
            }
        }
    }

    fn remember_selection(&mut self) {
        if let Some(entry) = self.selected_entry() {
            self.selector_memory
                .insert(self.current_dir.clone(), entry.path.clone());
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor =
            (self.cursor as isize + delta).clamp(0, self.entries.len() as isize - 1) as usize;
        if self.browser_loading {
            self.browser_user_navigated = true;
        }
    }

    fn open_selected_table(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry() else {
            return AppMode::Browser;
        };
        if entry.path.is_dir() {
            self.go_to(entry.path.clone());
            AppMode::Browser
        } else {
            self.open_external(false)
        }
    }

    fn activate_tree_entry(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        if entry.kind == EntryKind::Directory {
            if !self.expanded_directories.remove(&entry.path) {
                self.expanded_directories.insert(entry.path.clone());
            }
            self.refresh_browser(Some(entry.path));
            AppMode::Browser
        } else {
            self.open_external(false)
        }
    }

    fn tree_right(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        if entry.kind != EntryKind::Directory {
            return self.open_external(false);
        }
        let depth = self.tree_depth(self.cursor);
        if self.expanded_directories.insert(entry.path.clone()) {
            self.refresh_browser(Some(entry.path));
        } else if self
            .tree_depths
            .get(self.cursor + 1)
            .is_some_and(|child_depth| *child_depth > depth)
        {
            self.cursor += 1;
        }
        AppMode::Browser
    }

    fn tree_left(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.go_parent();
            return;
        };
        if entry.kind == EntryKind::Directory && self.expanded_directories.remove(&entry.path) {
            self.refresh_browser(Some(entry.path));
            return;
        }
        let depth = self.tree_depth(self.cursor);
        if depth > 0 {
            if let Some(parent_index) = (0..self.cursor)
                .rev()
                .find(|index| self.tree_depth(*index) + 1 == depth)
            {
                self.cursor = parent_index;
            }
        } else {
            self.go_parent();
        }
    }

    fn go_parent(&mut self) {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let previous_root = self.current_dir.clone();
        self.remember_selection();
        self.current_dir = parent;
        self.expanded_directories.clear();
        self.cursor = 0;
        self.refresh_browser(Some(previous_root));
    }

    fn go_to(&mut self, path: PathBuf) {
        self.remember_selection();
        let path = if path.is_absolute() {
            path
        } else {
            self.current_dir.join(path)
        };
        if path.is_dir() {
            self.current_dir = path.canonicalize().unwrap_or(path);
            self.expanded_directories.clear();
            self.cursor = 0;
            self.refresh_browser(None);
        } else {
            self.status = format!("Not a directory: {}", path.display());
        }
    }

    fn search_here(&mut self, query: &str) {
        self.search_filter = Some(query.to_lowercase());
        if cfg!(test) {
            self.refresh();
            if self.entries.is_empty() {
                self.search_filter = None;
                self.refresh();
                self.status = format!("No match for {query}");
            } else {
                self.set_notice(format!("Search: {} match(es)", self.entries.len()));
            }
        } else {
            self.pending_directory_search = Some(query.into());
            self.refresh();
        }
    }

    fn start_filesystem_search(&mut self, query: &str) {
        const SEARCH_QUEUE_CAPACITY: usize = 512;
        let (sender, receiver) = mpsc::sync_channel(SEARCH_QUEUE_CAPACITY);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let query_lower = query.to_lowercase();
        thread::spawn(move || {
            search_filesystem(Path::new("/"), &query_lower, &sender, &worker_cancel);
        });
        self.search_matches = 0;
        self.search_skipped = 0;
        self.search_cancelling = false;
        self.search = Some(RunningSearch {
            receiver,
            cancel,
            query: query.into(),
            results: Vec::new(),
            skipped: 0,
        });
    }

    fn start_update_check(&mut self) {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(updater::check(env!("CARGO_PKG_VERSION")));
        });
        self.update_check = Some(RunningUpdateCheck { receiver });
    }

    fn start_update(&mut self, version: &str) -> bool {
        let version = version.to_string();
        let executable = match env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.status = format!("Could not locate the installed binary: {error}");
                return false;
            }
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = updater::install(&version, &executable).map(|()| version);
            let _ = sender.send(result);
        });
        self.update = Some(RunningUpdate { receiver });
        true
    }

    fn open_search_result(&mut self, path: &Path) {
        self.search_filter = None;
        if path.is_dir() {
            self.go_to(path.to_path_buf());
            return;
        }
        let Some(parent) = path.parent() else {
            self.status = format!("Cannot open {}", path.display());
            return;
        };
        self.remember_selection();
        self.current_dir = parent.to_path_buf();
        self.expanded_directories.clear();
        self.cursor = 0;
        self.refresh_browser(Some(path.to_path_buf()));
    }

    fn toggle_selection(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.selected = !entry.selected;
        }
    }

    fn cycle_sort(&mut self) {
        self.config.ui.sort = match self.config.ui.sort {
            SortSetting::Name => SortSetting::Extension,
            SortSetting::Extension => SortSetting::Size,
            SortSetting::Size => SortSetting::Modified,
            SortSetting::Modified => SortSetting::Type,
            SortSetting::Type => SortSetting::Permissions,
            SortSetting::Permissions => SortSetting::Name,
        };
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let selected: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect();
        if selected.is_empty() {
            self.selected_entry()
                .map(|entry| vec![entry.path.clone()])
                .unwrap_or_default()
        } else {
            selected
        }
    }

    fn mutation_targets(&mut self) -> Option<Vec<PathBuf>> {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: file operations are disabled".into();
            return None;
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            None
        } else {
            Some(paths)
        }
    }

    fn set_clipboard(&mut self, mode: ClipboardMode) {
        let Some(paths) = self.mutation_targets() else {
            return;
        };
        let count = paths.len();
        self.clipboard = Some(Clipboard { mode, paths });
        self.set_notice(format!("{count} item(s) placed in file clipboard"));
    }

    fn prepare_paste(&mut self) -> AppMode {
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

    fn start_copy(&mut self, sources: Vec<PathBuf>, cut: bool, overwrite: bool) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = None;
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

    fn start_trash(&mut self, paths: Vec<PathBuf>) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = None;
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

    fn start_permanent_delete(&mut self, entries: Vec<TrashEntry>, manager: TrashManager) {
        self.progress = ProgressState::default();
        self.progress.cancellable = true;
        self.operation_trash_manager = Some(manager.clone());
        self.operation = Some(operation::spawn(OperationRequest::PermanentlyDelete {
            entries,
            manager,
        }));
    }

    fn start_luks(&mut self, action: LuksAction, retry: Option<LuksRetry>) {
        let (label, current) = match &action {
            LuksAction::UnlockAndMount { source, .. } => {
                ("Unlocking and mounting volume", source.clone())
            }
            LuksAction::Mount { mapping } => ("Mounting volume", mapping.clone()),
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

    fn create_directory(&mut self, name: &str) {
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

    fn create_file(&mut self, name: &str) -> AppMode {
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

    fn rename(&mut self, source: &Path, name: &str) {
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
        self.refresh_browser(renamed.then_some(destination));
    }

    fn open_trash(&mut self) -> AppMode {
        match TrashManager::for_path(&self.current_dir) {
            Ok(manager) => self.open_trash_manager(manager),
            Err(error) => AppMode::Prompt(Prompt::Message {
                title: "Trash unavailable".into(),
                body: error.to_string(),
            }),
        }
    }

    fn open_trash_manager(&mut self, manager: TrashManager) -> AppMode {
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

    fn open_devices(&mut self) -> AppMode {
        if cfg!(test) {
            self.last_device_refresh = Instant::now();
            return match luks::discover() {
                Ok(devices) => AppMode::Devices(DeviceView {
                    devices,
                    selected: 0,
                }),
                Err(error) => AppMode::Prompt(Prompt::Message {
                    title: "Encrypted devices unavailable".into(),
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

    fn open_external(&mut self, editor: bool) -> AppMode {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: external opening is disabled".into();
            return AppMode::Browser;
        }
        let Some(entry) = self.selected_entry() else {
            return AppMode::Browser;
        };
        if editor && !entry.is_text_file() {
            return AppMode::Browser;
        }
        let path = entry.path.clone();
        let program = if editor {
            launcher::resolve_editor(&self.config.open.editor)
        } else {
            self.config.open.opener.clone()
        };
        if editor && launcher::is_terminal_editor(&program) {
            let selected_paths = self
                .entries
                .iter()
                .filter(|entry| entry.selected)
                .map(|entry| entry.path.clone())
                .collect();
            self.pending_terminal_editor = Some(PendingTerminalEditor {
                program,
                path: path.clone(),
                browser: Some(BrowserSnapshot {
                    cursor_path: path,
                    selected_paths,
                }),
            });
            return AppMode::Browser;
        }
        if let Err(error) = launcher::launch(program, path, self.launch_sender.clone()) {
            return AppMode::Prompt(Prompt::OpenError {
                body: error.to_string(),
                config_error: None,
            });
        }
        AppMode::Browser
    }
}

pub(crate) fn format_elapsed(duration: Duration) -> String {
    let total_tenths = duration.as_millis() / 100;
    let minutes = total_tenths / 600;
    let seconds = total_tenths % 600;
    if minutes > 0 {
        format!("{minutes}m {}.{:01}s", seconds / 10, seconds % 10)
    } else {
        format!("{}.{:01}s", seconds / 10, seconds % 10)
    }
}

fn trash_targets(view: &TrashView) -> Vec<TrashEntry> {
    let marked = view
        .entries
        .iter()
        .filter(|entry| view.marked.contains(&entry.trashed_path))
        .cloned()
        .collect::<Vec<_>>();
    if marked.is_empty() {
        view.entries
            .get(view.selected)
            .cloned()
            .into_iter()
            .collect()
    } else {
        marked
    }
}

fn edit_input(input: &mut String, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => true,
        KeyCode::Backspace => {
            input.pop();
            false
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            input.push(ch);
            false
        }
        _ => false,
    }
}

fn edit_cursor_input(input: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    let character_count = input.chars().count();
    *cursor = (*cursor).min(character_count);
    match key.code {
        KeyCode::Esc => true,
        KeyCode::Left => {
            *cursor = cursor.saturating_sub(1);
            false
        }
        KeyCode::Right => {
            *cursor = (*cursor + 1).min(input.chars().count());
            false
        }
        KeyCode::Home => {
            *cursor = 0;
            false
        }
        KeyCode::End => {
            *cursor = input.chars().count();
            false
        }
        KeyCode::Backspace if *cursor > 0 => {
            let start = character_byte_index(input, *cursor - 1);
            let end = character_byte_index(input, *cursor);
            input.replace_range(start..end, "");
            *cursor -= 1;
            false
        }
        KeyCode::Delete if *cursor < input.chars().count() => {
            let start = character_byte_index(input, *cursor);
            let end = character_byte_index(input, *cursor + 1);
            input.replace_range(start..end, "");
            false
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let index = character_byte_index(input, *cursor);
            input.insert(index, character);
            *cursor += 1;
            false
        }
        _ => false,
    }
}

fn character_byte_index(input: &str, character_index: usize) -> usize {
    input
        .char_indices()
        .nth(character_index)
        .map(|(index, _)| index)
        .unwrap_or(input.len())
}

fn expand_home(input: &str) -> String {
    if input == "~" {
        return env::var("HOME").unwrap_or_else(|_| input.into());
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    input.into()
}

fn search_filesystem(
    root: &Path,
    query: &str,
    sender: &SyncSender<SearchUpdate>,
    cancel: &AtomicBool,
) {
    const MAX_RESULTS: usize = 10_000;
    let mut pending = vec![root.to_path_buf()];
    let mut result_count = 0;
    let mut cancelled = false;
    let mut limited = false;

    while let Some(directory) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if is_virtual_search_path(&directory) {
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                let _ = sender.send(SearchUpdate::PermissionDenied);
                continue;
            }
        };
        for item in entries {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let item = match item {
                Ok(item) => item,
                Err(_) => {
                    let _ = sender.send(SearchUpdate::PermissionDenied);
                    continue;
                }
            };
            let file_type = match item.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    let _ = sender.send(SearchUpdate::PermissionDenied);
                    continue;
                }
            };
            let file_name = item.file_name();
            let matches = entry::contains_case_insensitive(&file_name.to_string_lossy(), query);
            let is_directory = file_type.is_dir() && !file_type.is_symlink();
            if is_directory {
                let path = item.path();
                pending.push(path.clone());
                if !matches {
                    continue;
                }
                if result_count >= MAX_RESULTS {
                    limited = true;
                    break;
                }
                result_count += 1;
                let _ = sender.send(SearchUpdate::Match(path));
            } else if matches {
                if result_count >= MAX_RESULTS {
                    limited = true;
                    break;
                }
                result_count += 1;
                let _ = sender.send(SearchUpdate::Match(item.path()));
            }
        }
        if cancelled || limited {
            break;
        }
    }
    let _ = sender.send(SearchUpdate::Finished { cancelled, limited });
}

fn is_virtual_search_path(path: &Path) -> bool {
    if path == Path::new("/run") || path.starts_with("/run/media") {
        return false;
    }
    ["/proc", "/sys", "/dev", "/run"]
        .iter()
        .map(Path::new)
        .any(|excluded| path == excluded || path.starts_with(excluded))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn modal_isolation_prevents_browser_shortcuts() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("untouched.txt");
        std::fs::write(&file, b"safe").unwrap();
        let mut app = test_app(temp.path());
        app.mode = AppMode::Prompt(Prompt::GoTo {
            input: String::new(),
        });

        for ch in ['d', 'D', 'x', 'c', 'p', 'r', 'm', 'v', 'q'] {
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
            .try_send(LaunchError {
                program: "missing-opener".into(),
                path: temp.path().join("example.txt"),
                detail: "application not found".into(),
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
            .try_send(LaunchError {
                program: "missing-editor".into(),
                path: path.clone(),
                detail: "application not found".into(),
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
    fn current_directory_search_filters_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Report.txt"), b"report").unwrap();
        std::fs::write(temp.path().join("notes.txt"), b"notes").unwrap();
        let mut app = test_app(temp.path());

        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "report".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].name, "Report.txt");
        assert_eq!(app.search_filter.as_deref(), Some("report"));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search_filter.is_none());
        assert_eq!(app.entries.len(), 2);
    }

    #[test]
    fn tree_search_restores_hierarchy_and_nested_selector() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        let file = child.join("notes.txt");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(&file, b"notes").unwrap();
        let mut app = test_app(temp.path());

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "notes".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.selected_entry().unwrap().path, file);
        assert_eq!(app.tree_depth(app.cursor), 0);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.is_tree_directory_expanded(&child));
        assert_eq!(app.selected_entry().unwrap().path, file);
        assert_eq!(app.tree_depth(app.cursor), 1);
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
        app.search_filter = Some("target".into());
        app.request_browser_load(Some(target.clone()));

        let deadline = Instant::now() + Duration::from_secs(5);
        while app.browser_loading && Instant::now() < deadline {
            app.poll_browser_load();
            thread::sleep(Duration::from_millis(1));
        }

        assert!(!app.browser_loading);
        assert_eq!(app.entries.len(), 1);
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
    fn filesystem_search_returns_nested_full_paths() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let target = nested.join("Report.txt");
        std::fs::write(&target, b"report").unwrap();
        let (sender, receiver) = mpsc::sync_channel(512);
        let cancel = AtomicBool::new(false);

        search_filesystem(temp.path(), "report", &sender, &cancel);
        drop(sender);
        let updates = receiver.into_iter().collect::<Vec<_>>();

        assert!(updates
            .iter()
            .any(|update| matches!(update, SearchUpdate::Match(path) if path == &target)));
        assert!(matches!(
            updates.last(),
            Some(SearchUpdate::Finished {
                cancelled: false,
                ..
            })
        ));
    }

    #[test]
    fn filesystem_search_honors_cancellation_before_traversal() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("needle.txt"), b"needle").unwrap();
        let (sender, receiver) = mpsc::sync_channel(8);
        let cancel = AtomicBool::new(true);

        search_filesystem(temp.path(), "needle", &sender, &cancel);

        assert!(matches!(
            receiver.recv().unwrap(),
            SearchUpdate::Finished {
                cancelled: true,
                limited: false
            }
        ));
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
    #[ignore]
    fn benchmark_filesystem_search() {
        let root = std::env::var_os("MINFM_PERF_SEARCH_DIR")
            .map(PathBuf::from)
            .expect("MINFM_PERF_SEARCH_DIR is required");
        let mut samples = Vec::new();
        for _ in 0..9 {
            let (sender, receiver) = mpsc::sync_channel(512);
            let cancel = AtomicBool::new(false);
            let started = Instant::now();
            search_filesystem(&root, "needle", &sender, &cancel);
            drop(sender);
            let matches = receiver
                .into_iter()
                .filter(|update| matches!(update, SearchUpdate::Match(_)))
                .count();
            assert_eq!(matches, 250);
            samples.push(started.elapsed());
        }
        samples.sort();
        eprintln!("PERF search_median_us={}", samples[4].as_micros());
    }

    #[test]
    fn u_does_not_open_the_disk_manager() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
    }
}
