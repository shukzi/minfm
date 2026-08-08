use std::{
    collections::{HashMap, HashSet},
    env,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    config::{self, Config, ConfigLoad, SortSetting},
    entry::{self, FileEntry},
    luks::{self, LuksAction, LuksDevice, LuksOutcome, SecretInput},
    operation::{self, OperationRequest, OperationSummary, OperationUpdate, RunningOperation},
    trash::{TrashEntry, TrashManager},
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
    Message {
        title: String,
        body: String,
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
    Trash(TrashView),
    Devices(DeviceView),
    Help,
    Info,
    ConfigError { path: PathBuf, error: String },
}

#[derive(Debug, Clone, Default)]
pub struct ProgressState {
    pub label: String,
    pub current: Option<PathBuf>,
    pub total_items: usize,
    pub completed_items: usize,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub cancelling: bool,
    pub cancellable: bool,
}

#[derive(Debug, Clone)]
pub enum PendingSystemAction {
    Editor {
        program: String,
        path: PathBuf,
        reload_config: bool,
    },
}

#[derive(Debug)]
pub enum SystemActionOutcome {
    EditorFinished {
        reload_config: bool,
        message: String,
    },
}

struct RunningLuks {
    receiver: Receiver<crate::error::Result<LuksOutcome>>,
    retry: Option<LuksRetry>,
}

struct LuksRetry {
    source: PathBuf,
    label: Option<String>,
    size: u64,
}

const STATUS_NOTICE_DURATION: Duration = Duration::from_secs(10);

pub struct App {
    pub running: bool,
    pub current_dir: PathBuf,
    pub entries: Vec<FileEntry>,
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
    pending_system_action: Option<PendingSystemAction>,
    last_device_refresh: Instant,
    selector_memory: HashMap<PathBuf, PathBuf>,
    loaded_dir: PathBuf,
    pub search_filter: Option<String>,
    search: Option<RunningSearch>,
    search_matches: usize,
    search_skipped: usize,
    search_cancelling: bool,
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
        let mut app = Self {
            running: true,
            current_dir: start,
            entries: Vec::new(),
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
            pending_system_action: None,
            last_device_refresh: Instant::now(),
            selector_memory: HashMap::new(),
            loaded_dir: start.clone(),
            search_filter: None,
            search: None,
            search_matches: 0,
            search_skipped: 0,
            search_cancelling: false,
        };
        if matches!(app.mode, AppMode::Browser) {
            app.refresh();
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
            AppMode::Trash(view) => self.handle_trash_key(view, key),
            AppMode::Devices(view) => self.handle_device_key(view, key),
            AppMode::Help => self.handle_readonly_popup(key, AppMode::Help),
            AppMode::Info => self.handle_readonly_popup(key, AppMode::Info),
            AppMode::ConfigError { path, error } => self.handle_config_error(path, error, key),
        };
    }

    pub fn poll_operation(&mut self) {
        let Some(operation) = &self.operation else {
            return;
        };
        let mut updates = Vec::new();
        while let Ok(update) = operation.receiver.try_recv() {
            updates.push(update);
        }
        for update in updates {
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
                    self.operation = None;
                    let return_to_trash = self.operation_trash_manager.take();
                    self.refresh();
                    if summary.failed.is_empty()
                        && summary.warnings.is_empty()
                        && !summary.cancelled
                    {
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
                }
            }
        }
    }

    pub fn poll_luks_operation(&mut self) {
        let retry = self.luks_operation.as_ref().and_then(|running| {
            running.retry.as_ref().map(|retry| LuksRetry {
                source: retry.source.clone(),
                label: retry.label.clone(),
                size: retry.size,
            })
        });
        let result = match self
            .luks_operation
            .as_ref()
            .map(|running| running.receiver.try_recv())
        {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Empty)) | None => return,
            Some(Err(TryRecvError::Disconnected)) => Err(crate::error::MinfmError::Message(
                "encrypted-volume worker stopped unexpectedly".into(),
            )),
        };
        self.luks_operation = None;
        match result {
            Ok(outcome) => {
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
                self.status = format!("Encrypted-volume operation failed: {error}");
                self.mode = self.open_devices();
            }
        }
    }

    pub fn poll_search(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let mut updates = Vec::new();
        while let Ok(update) = search.receiver.try_recv() {
            updates.push(update);
        }
        let mut finished = None;
        for update in updates {
            match update {
                SearchUpdate::Match(path) => {
                    if let Some(search) = &mut self.search {
                        search.results.push(path);
                        self.search_matches = search.results.len();
                    }
                }
                SearchUpdate::PermissionDenied => {
                    if let Some(search) = &mut self.search {
                        search.skipped += 1;
                        self.search_skipped = search.skipped;
                    }
                }
                SearchUpdate::Finished { cancelled, limited } => {
                    finished = Some((cancelled, limited))
                }
            }
        }
        let Some((cancelled, limited)) = finished else {
            return;
        };
        let search = self.search.take().expect("search exists while polling");
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
    }

    pub fn poll_devices(&mut self) {
        if !matches!(self.mode, AppMode::Devices(_))
            || self.last_device_refresh.elapsed() < Duration::from_secs(1)
        {
            return;
        }
        self.last_device_refresh = Instant::now();
        let selected_source = match &self.mode {
            AppMode::Devices(view) => view
                .devices
                .get(view.selected)
                .map(|device| device.source.clone()),
            _ => None,
        };
        match luks::discover() {
            Ok(devices) => {
                let selected = selected_source
                    .and_then(|source| devices.iter().position(|device| device.source == source))
                    .unwrap_or(0);
                self.mode = AppMode::Devices(DeviceView { devices, selected });
            }
            Err(error) => self.status = format!("Automatic disk refresh failed: {error}"),
        }
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.cursor)
    }

    pub fn take_system_action(&mut self) -> Option<PendingSystemAction> {
        self.pending_system_action.take()
    }

    pub fn finish_system_action(
        &mut self,
        action: &PendingSystemAction,
        result: crate::error::Result<SystemActionOutcome>,
    ) {
        match result {
            Ok(SystemActionOutcome::EditorFinished {
                reload_config,
                message,
            }) => {
                self.set_notice(message);
                if reload_config {
                    self.mode = match config::load_from(self.config_path.clone()) {
                        ConfigLoad::Valid { config, path } => {
                            self.config = config;
                            self.config_path = path;
                            self.refresh();
                            self.set_notice("Configuration reloaded");
                            AppMode::Browser
                        }
                        ConfigLoad::Invalid { path, error } => AppMode::ConfigError { path, error },
                    };
                } else {
                    self.refresh();
                    self.mode = AppMode::Browser;
                }
            }
            Err(error) => {
                self.status = format!("System operation failed: {error}");
                self.mode = match action {
                    PendingSystemAction::Editor {
                        reload_config: true,
                        ..
                    } => match config::load_from(self.config_path.clone()) {
                        ConfigLoad::Valid { config, path } => {
                            self.config = config;
                            self.config_path = path;
                            self.refresh();
                            AppMode::Browser
                        }
                        ConfigLoad::Invalid { path, error } => AppMode::ConfigError { path, error },
                    },
                    PendingSystemAction::Editor { .. } => AppMode::Browser,
                };
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
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Left | KeyCode::Char('h') => self.go_parent(),
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
                self.open_external(key.code == KeyCode::Char('e'))
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
            Prompt::Message { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    return AppMode::Browser;
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
            KeyCode::Char('r') => self.open_devices(),
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
                    AppMode::Prompt(Prompt::ConfirmLuks {
                        action: LuksAction::UnmountAndLock {
                            source: device.source.clone(),
                            mapping: device.mapping.clone().expect("unlocked device has mapping"),
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
                    AppMode::Prompt(Prompt::ConfirmLuks {
                        action: LuksAction::Mount {
                            mapping: device.mapping.clone().expect("unlocked device has mapping"),
                        },
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
                let editor = resolve_editor(&self.config.open.editor);
                self.pending_system_action = Some(PendingSystemAction::Editor {
                    program: editor,
                    path,
                    reload_config: true,
                });
                AppMode::Progress
            }
            _ => AppMode::ConfigError { path, error },
        }
    }

    fn refresh(&mut self) {
        if self.loaded_dir == self.current_dir {
            self.remember_selection();
        }
        match entry::read_directory(
            &self.current_dir,
            self.config.ui.show_hidden,
            self.config.ui.sort,
            self.config.ui.reverse_sort,
            self.config.ui.directories_first,
        ) {
            Ok(entries) => {
                let query = self.search_filter.as_deref();
                self.entries = entries
                    .into_iter()
                    .filter(|entry| {
                        query.is_none_or(|query| {
                            entry.name.to_lowercase().contains(query)
                        })
                    })
                    .collect();
                self.cursor = self
                    .selector_memory
                    .get(&self.current_dir)
                    .and_then(|path| self.entries.iter().position(|entry| &entry.path == path))
                    .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
                self.loaded_dir = self.current_dir.clone();
            }
            Err(error) => self.status = error.to_string(),
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
    }

    fn open_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.path.is_dir() {
            self.go_to(entry.path.clone());
        } else {
            self.open_external(false);
        }
    }

    fn go_parent(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.cursor = 0;
            self.refresh();
        }
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
            self.cursor = 0;
            self.refresh();
        } else {
            self.status = format!("Not a directory: {}", path.display());
        }
    }

    fn search_here(&mut self, query: &str) {
        self.search_filter = Some(query.to_lowercase());
        self.refresh();
        if self.entries.is_empty() {
            self.search_filter = None;
            self.refresh();
            self.status = format!("No match for {query}");
        } else {
            self.set_notice(format!("Search: {} match(es)", self.entries.len()));
        }
    }

    fn start_filesystem_search(&mut self, query: &str) {
        let (sender, receiver) = mpsc::channel();
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
        self.go_to(parent.to_path_buf());
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            self.cursor = index;
        } else {
            self.status = format!("Search result is no longer available: {}", path.display());
        }
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
        self.progress = ProgressState {
            label: label.into(),
            current: Some(current),
            cancellable: false,
            ..ProgressState::default()
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(luks::execute(&action));
        });
        self.luks_operation = Some(RunningLuks { receiver, retry });
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
        match std::fs::rename(source, &destination) {
            Ok(()) => self.set_notice(format!("Renamed to {}", destination.display())),
            Err(error) => self.status = format!("Rename failed: {error}"),
        }
        self.refresh();
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
        self.last_device_refresh = Instant::now();
        match luks::discover() {
            Ok(devices) => AppMode::Devices(DeviceView {
                devices,
                selected: 0,
            }),
            Err(error) => AppMode::Prompt(Prompt::Message {
                title: "Encrypted devices unavailable".into(),
                body: error.to_string(),
            }),
        }
    }

    fn open_external(&mut self, editor: bool) {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: external opening is disabled".into();
            return;
        }
        let Some(entry) = self.selected_entry() else {
            return;
        };
        let path = entry.path.clone();
        if editor {
            self.pending_system_action = Some(PendingSystemAction::Editor {
                program: resolve_editor(&self.config.open.editor),
                path,
                reload_config: false,
            });
        } else {
            match Command::new(&self.config.open.opener).arg(&path).spawn() {
                Ok(_) => self.set_notice(format!("Opened {}", path.display())),
                Err(error) => self.status = format!("Could not open {}: {error}", path.display()),
            }
        }
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

fn resolve_editor(configured: &str) -> String {
    if configured == "$EDITOR" {
        env::var("EDITOR").unwrap_or_else(|_| "vi".into())
    } else {
        configured.into()
    }
}

fn search_filesystem(
    root: &Path,
    query: &str,
    sender: &Sender<SearchUpdate>,
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
            let path = item.path();
            let file_type = match item.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    let _ = sender.send(SearchUpdate::PermissionDenied);
                    continue;
                }
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                pending.push(path.clone());
            }
            if path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase().contains(query))
                .unwrap_or(false)
            {
                if result_count >= MAX_RESULTS {
                    limited = true;
                    break;
                }
                result_count += 1;
                let _ = sender.send(SearchUpdate::Match(path));
            }
        }
        if cancelled || limited {
            break;
        }
    }
    let _ = sender.send(SearchUpdate::Finished { cancelled, limited });
}

fn is_virtual_search_path(path: &Path) -> bool {
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

        for ch in ['d', 'D', 'x', 'c', 'p', 'r', 'm', 'q'] {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        assert!(file.exists());
        assert!(app.operation.is_none());
        assert!(app.running);
        assert!(matches!(app.mode, AppMode::Prompt(Prompt::GoTo { .. })));
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
    fn parent_navigation_restores_the_previous_selector() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let mut app = test_app(temp.path());
        app.cursor = app
            .entries
            .iter()
            .position(|entry| entry.path == second)
            .unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.current_dir, second);
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));

        assert_eq!(app.current_dir, temp.path());
        assert_eq!(app.selected_entry().unwrap().path, second);
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
    fn filesystem_search_returns_nested_full_paths() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let target = nested.join("Report.txt");
        std::fs::write(&target, b"report").unwrap();
        let (sender, receiver) = mpsc::channel();
        let cancel = AtomicBool::new(false);

        search_filesystem(temp.path(), "report", &sender, &cancel);
        let updates = receiver.into_iter().collect::<Vec<_>>();

        assert!(updates
            .iter()
            .any(|update| matches!(update, SearchUpdate::Match(path) if path == &target)));
        assert!(matches!(updates.last(), Some(SearchUpdate::Finished { cancelled: false, .. })));
    }

    #[test]
    fn u_does_not_open_the_disk_manager() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
    }
}
