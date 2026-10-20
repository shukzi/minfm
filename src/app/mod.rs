use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::Ordering,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    archive::{
        self, ArchiveEntry, ArchiveFormat, ArchiveOutcome, ArchiveRequest, ArchiveUpdate,
        RunningArchive,
    },
    browser_loader::{self, LoadRequest, LoadUpdate, RunningLoad},
    config::{self, Config, ConfigLoad, SortSetting},
    entry::{self, EntryKind, FileEntry},
    launcher::{self, LaunchError},
    luks::{self, LuksAction, LuksDevice, LuksOutcome, SecretInput},
    network::{
        self, ConnectRequest, NetworkAction, NetworkAuth, NetworkEnvironment, NetworkOutcome,
        NetworkSecret, NetworkShare, ShareAddress,
    },
    operation::{self, OperationRequest, OperationSummary, OperationUpdate, RunningOperation},
    partition::{
        self, DeviceIdentity, Filesystem, PartitionAction, PartitionEntry, PartitionInventory,
        PartitionTable,
    },
    search::{
        self, CompiledSearch, SearchDraft, SearchHit, SearchScope, SearchUpdate,
        UPDATES_PER_UI_TICK,
    },
    trash::{TrashEntry, TrashManager},
    updater,
};

mod browser;
mod device;
mod file_flow;
mod input;
mod network_flow;
mod partition_input;
mod partition_menu;
mod polling;
mod search_flow;
mod update_flow;

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
    ArchiveFormat {
        sources: Vec<PathBuf>,
        selected: usize,
    },
    ArchiveName {
        sources: Vec<PathBuf>,
        format: ArchiveFormat,
        input: String,
        cursor: usize,
    },
    ArchiveActions {
        archive: PathBuf,
        selected: usize,
    },
    ArchiveDestination {
        archive: PathBuf,
        input: String,
        cursor: usize,
    },
    ConfirmTrash {
        paths: Vec<PathBuf>,
    },
    ConfirmOverwrite {
        sources: Vec<PathBuf>,
        destination: PathBuf,
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
    PartitionAuthentication {
        action: PartitionAction,
        view: PartitionView,
        input: SecretInput,
        error: Option<String>,
    },
    PartitionError {
        body: String,
        view: PartitionView,
    },
    Mounted {
        path: PathBuf,
    },
    SmbAddress {
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    SmbUsername {
        address: ShareAddress,
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    SmbDomain {
        address: ShareAddress,
        username: String,
        input: String,
        cursor: usize,
    },
    SmbPassword {
        address: ShareAddress,
        username: String,
        domain: String,
        input: NetworkSecret,
        error: Option<String>,
    },
    SmbRemember {
        request: ConnectRequest,
        available: bool,
    },
    ConfirmSmbDisconnect {
        share: NetworkShare,
    },
    ConfirmSmbForget {
        share: NetworkShare,
    },
    SmbMounted {
        address: ShareAddress,
        path: PathBuf,
    },
    SmbMessage {
        title: String,
        body: String,
        return_to_network: bool,
    },
    UpdateAvailable {
        current: String,
        latest: String,
    },
    Message {
        title: String,
        body: String,
    },
    SmartReport {
        body: String,
        scroll: u16,
        view: PartitionView,
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

#[allow(dead_code)] // Task 5 consumes the full section navigation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSection {
    Scope,
    Match,
    Filters,
    Traversal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchReturn {
    Browser,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnDestination {
    Browser,
    SearchResults,
    Tools,
}

#[derive(Debug)]
struct PendingLaunchError {
    error: LaunchError,
    return_to: ReturnDestination,
}

impl ReturnDestination {
    fn mode(self) -> AppMode {
        match self {
            Self::Browser => AppMode::Browser,
            Self::SearchResults => AppMode::SearchResults,
            Self::Tools => AppMode::Tools(ToolsView { selected: 0 }),
        }
    }
}

impl SearchReturn {
    fn mode(self) -> AppMode {
        match self {
            Self::Browser => AppMode::Browser,
            Self::Results => AppMode::SearchResults,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchForm {
    pub draft: SearchDraft,
    pub advanced: bool,
    #[allow(dead_code)] // Task 5 owns full field navigation.
    pub section: SearchSection,
    #[allow(dead_code)] // Task 5 owns full field navigation.
    pub field: usize,
    pub cursors: SearchCursors,
    pub error: Option<String>,
    pub return_to: SearchReturn,
}

#[derive(Debug, Clone, Default)]
pub struct SearchCursors {
    pub name: usize,
    pub content: usize,
    pub minimum_size: usize,
    pub maximum_size: usize,
    pub modified_after: usize,
    pub modified_before: usize,
}

impl SearchForm {
    fn quick(root: PathBuf, return_to: SearchReturn) -> Self {
        Self {
            draft: SearchDraft::quick(root),
            advanced: false,
            section: SearchSection::Match,
            field: 0,
            cursors: SearchCursors::default(),
            error: None,
            return_to,
        }
    }

    pub(crate) fn advanced(root: PathBuf, scope: SearchScope, return_to: SearchReturn) -> Self {
        Self {
            draft: SearchDraft::advanced(root, scope),
            advanced: true,
            section: SearchSection::Scope,
            field: 0,
            cursors: SearchCursors::default(),
            error: None,
            return_to,
        }
    }

    fn edit_active_text(&mut self, key: KeyEvent) -> Option<bool> {
        let (input, cursor) = match (self.section, self.field) {
            (SearchSection::Match, 1) => (&mut self.draft.content, &mut self.cursors.content),
            (SearchSection::Filters, 5) => {
                (&mut self.draft.minimum_size, &mut self.cursors.minimum_size)
            }
            (SearchSection::Filters, 6) => {
                (&mut self.draft.maximum_size, &mut self.cursors.maximum_size)
            }
            (SearchSection::Filters, 7) => (
                &mut self.draft.modified_after,
                &mut self.cursors.modified_after,
            ),
            (SearchSection::Filters, 8) => (
                &mut self.draft.modified_before,
                &mut self.cursors.modified_before,
            ),
            _ => return None,
        };
        let before = input.clone();
        edit_cursor_input(input, cursor, key);
        Some(*input != before)
    }

    fn handle_choice_key(&mut self, key: KeyEvent) -> bool {
        use search::{ContentMode, NameMode, ResultLimit};
        let forward = key.code == KeyCode::Right;
        if !matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
        ) {
            return false;
        }
        if key.code == KeyCode::Char(' ') {
            if let (SearchSection::Filters, field @ 0..=4) = (self.section, self.field) {
                let kind = [
                    crate::entry::EntryKind::File,
                    crate::entry::EntryKind::Directory,
                    crate::entry::EntryKind::Symlink,
                    crate::entry::EntryKind::BlockDevice,
                    crate::entry::EntryKind::Other,
                ][field];
                self.draft.types.toggle(kind);
                return true;
            }
            return false;
        }
        match (self.section, self.field) {
            (SearchSection::Scope, 0) => {
                self.draft.scope = match (self.draft.scope, forward) {
                    (SearchScope::CurrentDirectory, true) => SearchScope::RecursiveHere,
                    (SearchScope::RecursiveHere, true) => SearchScope::Filesystem,
                    (SearchScope::Filesystem, true) => SearchScope::CurrentDirectory,
                    (SearchScope::CurrentDirectory, false) => SearchScope::Filesystem,
                    (SearchScope::RecursiveHere, false) => SearchScope::CurrentDirectory,
                    (SearchScope::Filesystem, false) => SearchScope::RecursiveHere,
                };
            }
            (SearchSection::Match, 0) => {
                self.draft.name_mode = match (self.draft.name_mode, forward) {
                    (NameMode::Smart, true) => NameMode::Glob,
                    (NameMode::Glob, true) => NameMode::Regex,
                    (NameMode::Regex, true) => NameMode::Smart,
                    (NameMode::Smart, false) => NameMode::Regex,
                    (NameMode::Glob, false) => NameMode::Smart,
                    (NameMode::Regex, false) => NameMode::Glob,
                };
            }
            (SearchSection::Match, 2) => {
                self.draft.content_mode = match self.draft.content_mode {
                    ContentMode::Literal => ContentMode::Regex,
                    ContentMode::Regex => ContentMode::Literal,
                };
            }
            (SearchSection::Filters, 9) => {
                self.draft.include_ignored_hidden = !self.draft.include_ignored_hidden;
            }
            (SearchSection::Traversal, 0) => {
                self.draft.result_limit = match (self.draft.result_limit, forward) {
                    (ResultLimit::OneThousand, true) => ResultLimit::FiveThousand,
                    (ResultLimit::FiveThousand, true) => ResultLimit::TenThousand,
                    (ResultLimit::TenThousand, true) => ResultLimit::OneThousand,
                    (ResultLimit::OneThousand, false) => ResultLimit::TenThousand,
                    (ResultLimit::FiveThousand, false) => ResultLimit::OneThousand,
                    (ResultLimit::TenThousand, false) => ResultLimit::FiveThousand,
                };
            }
            _ => return false,
        }
        true
    }
}

impl SearchSection {
    fn moved(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Scope, true) => Self::Match,
            (Self::Match, true) => Self::Filters,
            (Self::Filters, true) => Self::Traversal,
            (Self::Traversal, true) => Self::Scope,
            (Self::Scope, false) => Self::Traversal,
            (Self::Match, false) => Self::Scope,
            (Self::Filters, false) => Self::Match,
            (Self::Traversal, false) => Self::Filters,
        }
    }

    fn field_count(self) -> usize {
        match self {
            Self::Scope | Self::Traversal => 1,
            Self::Match => 3,
            Self::Filters => 10,
        }
    }
}

#[derive(Debug)]
pub struct SearchView {
    pub request: CompiledSearch,
    pub results: Vec<SearchHit>,
    pub selected: usize,
    pub selected_path: Option<PathBuf>,
    pub skipped: usize,
    pub truncated: bool,
    pub incomplete: bool,
}

#[derive(Debug, Clone)]
pub struct ArchiveView {
    pub archive: PathBuf,
    pub entries: Vec<ArchiveEntry>,
    pub selected: usize,
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

struct RunningNetworkRefresh {
    receiver: Receiver<NetworkRefreshUpdate>,
    selected_uri: Option<String>,
}

enum NetworkRefreshUpdate {
    Snapshot {
        result: Result<Vec<NetworkShare>, String>,
        finished: bool,
        secret_storage: Option<bool>,
    },
}

struct RunningNetworkOperation {
    receiver: Receiver<Result<NetworkOutcome, String>>,
}

struct RunningPartitionRefresh {
    receiver: Receiver<Result<PartitionInventory, String>>,
    selected_path: Option<PathBuf>,
}

enum PartitionUpdate {
    Phase {
        label: &'static str,
        started_at: Instant,
    },
    Finished(crate::error::Result<String>),
}

struct RunningPartitionOperation {
    receiver: Receiver<PartitionUpdate>,
    started_at: Instant,
    action: PartitionAction,
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

#[derive(Debug, Clone)]
pub struct NetworkView {
    pub shares: Vec<NetworkShare>,
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTool {
    DeviceManager,
    NetworkShares,
}

impl BuiltinTool {
    pub const ALL: [Self; 2] = [Self::DeviceManager, Self::NetworkShares];

    pub fn name(self) -> &'static str {
        match self {
            Self::DeviceManager => "Device manager",
            Self::NetworkShares => "Network shares",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::DeviceManager => {
                "Inspect, mount, unlock, unmount, lock, and safely eject storage"
            }
            Self::NetworkShares => "Discover, connect, and manage Samba shares",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolsView {
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerReturn {
    Files,
    Tools,
}

#[derive(Debug, Clone)]
pub struct PartitionView {
    pub entries: Vec<PartitionEntry>,
    pub selected: usize,
    pub overlay: Option<PartitionOverlay>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PartitionTask {
    Mount,
    Unmount,
    EncryptionAccess,
    ChangePassphrase,
    MountOptions,
    EncryptionOptions,
    Eject,
    SmartReport,
    SmartShortTest,
    SmartExtendedTest,
    DriveSettings,
    CreatePartition,
    Resize,
    Format,
    CreateTable,
    Delete,
    Label,
    Check,
    Repair,
    CreateImage,
    RestoreImage,
    Flag,
    PartitionName,
    PartitionType,
    BackupTable,
}

impl PartitionTask {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mount => "Mount",
            Self::Unmount => "Unmount",
            Self::EncryptionAccess => "Unlock or lock",
            Self::ChangePassphrase => "Change LUKS passphrase",
            Self::MountOptions => "Mount options",
            Self::EncryptionOptions => "Encryption options",
            Self::Eject => "Eject",
            Self::SmartReport => "SMART data",
            Self::SmartShortTest => "Short SMART test",
            Self::SmartExtendedTest => "Extended SMART test",
            Self::DriveSettings => "Drive settings",
            Self::CreatePartition => "Create partition",
            Self::Resize => "Resize",
            Self::Format => "Format",
            Self::CreateTable => "Format disk",
            Self::Delete => "Delete",
            Self::Label => "Edit filesystem",
            Self::Check => "Check filesystem",
            Self::Repair => "Repair filesystem",
            Self::CreateImage => "Create image",
            Self::RestoreImage => "Restore image",
            Self::Flag => "Set flag",
            Self::PartitionName => "Edit partition",
            Self::PartitionType => "Change type",
            Self::BackupTable => "Back up table",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Mount => "Make this filesystem available",
            Self::Unmount => "Safely stop using this filesystem",
            Self::EncryptionAccess => "Unlock, mount, unmount, or lock LUKS",
            Self::ChangePassphrase => "Replace the volume passphrase",
            Self::MountOptions => "Configure startup mount behavior",
            Self::EncryptionOptions => "Configure startup unlock behavior",
            Self::Eject => "Safely power off a removable drive",
            Self::SmartReport => "View health information",
            Self::SmartShortTest => "Start a quick drive self-test",
            Self::SmartExtendedTest => "Start a complete drive self-test",
            Self::DriveSettings => "Set standby, APM, AAM, or write cache",
            Self::CreatePartition => "Use free space",
            Self::Resize => "Change the partition size",
            Self::Format => "Erase and create a filesystem",
            Self::CreateTable => "Choose GPT, MBR, or no partition table",
            Self::Delete => "Remove this partition",
            Self::Label => "Change the filesystem label",
            Self::Check => "Run a read-only filesystem check",
            Self::Repair => "Repair filesystem errors",
            Self::CreateImage => "Save a full device image",
            Self::RestoreImage => "Replace the device from an image",
            Self::Flag => "Set boot, ESP, or another common flag",
            Self::PartitionName => "Change partition name and metadata",
            Self::PartitionType => "Change the partition type ID",
            Self::BackupTable => "Save a restorable table dump",
        }
    }

    pub fn risk(self) -> &'static str {
        match self {
            Self::Mount
            | Self::Unmount
            | Self::EncryptionAccess
            | Self::Eject
            | Self::SmartReport
            | Self::SmartShortTest
            | Self::SmartExtendedTest => "Safe",
            Self::ChangePassphrase | Self::MountOptions | Self::EncryptionOptions => {
                "Changes configuration"
            }
            Self::CreatePartition
            | Self::Resize
            | Self::Label
            | Self::Flag
            | Self::PartitionName
            | Self::PartitionType
            | Self::DriveSettings => "Changes settings",
            Self::Check | Self::BackupTable => "Read only",
            Self::Repair => "Changes data",
            Self::CreateImage => "Read only",
            Self::RestoreImage => "Erases data",
            Self::Format | Self::CreateTable | Self::Delete => "Erases data",
        }
    }
}

fn is_mbr_type(value: &str) -> bool {
    value.len() == 2 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            })
}

#[derive(Debug, Clone)]
pub enum PartitionOverlay {
    Actions {
        selected: usize,
    },
    FormatOptions {
        selected: usize,
        encrypted: bool,
        permissions: crate::partition::FilesystemPermissions,
    },
    EncryptionFilesystem {
        selected: usize,
        whole_disk: bool,
        permissions: crate::partition::FilesystemPermissions,
    },
    EncryptionPassphrase {
        filesystem: Filesystem,
        whole_disk: bool,
        label: Option<String>,
        passphrase: SecretInput,
        confirmation: SecretInput,
        confirming: bool,
        error: Option<String>,
        permissions: crate::partition::FilesystemPermissions,
    },
    ChangePassphrase {
        old: SecretInput,
        new: SecretInput,
        confirmation: SecretInput,
        stage: u8,
        error: Option<String>,
    },
    DiskLayoutOptions {
        selected: usize,
        overwrite: bool,
    },
    FreeRegionOptions {
        selected: usize,
    },
    PartitionSize {
        start_bytes: u64,
        maximum_end: u64,
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    FormatLabel {
        filesystem: Filesystem,
        encrypted: bool,
        permissions: crate::partition::FilesystemPermissions,
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    Input {
        task: PartitionTask,
        input: String,
        cursor: usize,
        hint: String,
        error: Option<String>,
    },
    Confirm {
        action: PartitionAction,
        yes_selected: bool,
    },
}

pub enum AppMode {
    Browser,
    SearchForm(SearchForm),
    Archive(ArchiveView),
    Tools(ToolsView),
    Prompt(Prompt),
    Progress,
    SearchProgress,
    SearchResults,
    UpdateProgress,
    Trash(TrashView),
    Devices(DeviceView),
    Network(NetworkView),
    NetworkProgress,
    Partitions(PartitionView),
    Help,
    Info(Option<FileEntry>),
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

struct PendingPartitionPreflight {
    view: PartitionView,
    task: PartitionTask,
    remaining: VecDeque<PathBuf>,
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
    return_to: ReturnDestination,
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
    archive_operation: Option<RunningArchive>,
    operation_trash_manager: Option<TrashManager>,
    operation_return: ReturnDestination,
    operation_search_paths: Vec<PathBuf>,
    operation_refresh_preferred: Option<PathBuf>,
    archive_return: ReturnDestination,
    modal_return: ReturnDestination,
    luks_operation: Option<RunningLuks>,
    launch_sender: SyncSender<PendingLaunchError>,
    launch_receiver: Receiver<PendingLaunchError>,
    pending_launch_errors: VecDeque<PendingLaunchError>,
    pending_terminal_editor: Option<PendingTerminalEditor>,
    last_device_refresh: Instant,
    device_refresh: Option<RunningDeviceRefresh>,
    pub device_refreshing: bool,
    network_environment: NetworkEnvironment,
    network_refresh: Option<RunningNetworkRefresh>,
    network_operation: Option<RunningNetworkOperation>,
    pub network_refreshing: bool,
    network_secret_storage_available: bool,
    partition_refresh: Option<RunningPartitionRefresh>,
    pub partition_refreshing: bool,
    partition_operation: Option<RunningPartitionOperation>,
    partition_return_view: Option<PartitionView>,
    partition_preflight: Option<PendingPartitionPreflight>,
    manager_return: ManagerReturn,
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
    search: Option<search::RunningSearch>,
    pub search_results: Option<SearchView>,
    previous_search_results: Option<SearchView>,
    search_return: SearchReturn,
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
        let network_environment = NetworkEnvironment::detect(&config_path);
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
            archive_operation: None,
            operation_trash_manager: None,
            operation_return: ReturnDestination::Browser,
            operation_search_paths: Vec::new(),
            operation_refresh_preferred: None,
            archive_return: ReturnDestination::Browser,
            modal_return: ReturnDestination::Browser,
            luks_operation: None,
            launch_sender,
            launch_receiver,
            pending_launch_errors: VecDeque::new(),
            pending_terminal_editor: None,
            last_device_refresh: Instant::now(),
            device_refresh: None,
            device_refreshing: false,
            network_environment,
            network_refresh: None,
            network_operation: None,
            network_refreshing: false,
            network_secret_storage_available: false,
            partition_refresh: None,
            partition_refreshing: false,
            partition_operation: None,
            partition_return_view: None,
            partition_preflight: None,
            manager_return: ManagerReturn::Files,
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
            search: None,
            search_results: None,
            previous_search_results: None,
            search_return: SearchReturn::Browser,
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

    pub fn poll_search(&mut self) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        let mut finished = None;
        let mut disconnected = false;
        let mut changed = false;
        let mut hits_changed = false;
        let selected_path = self.search_results.as_ref().and_then(|view| {
            view.selected_path.clone().or_else(|| {
                view.results
                    .get(view.selected)
                    .map(|hit| hit.entry.path.clone())
            })
        });
        for _ in 0..UPDATES_PER_UI_TICK {
            let update = match search.receiver.try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            };
            changed = true;
            match update {
                SearchUpdate::Match(hit) => {
                    let Some(view) = &mut self.search_results else {
                        continue;
                    };
                    insert_search_hit(&mut view.results, hit);
                    self.search_matches = view.results.len();
                    hits_changed = true;
                }
                SearchUpdate::Skipped(count) => {
                    let Some(view) = &mut self.search_results else {
                        continue;
                    };
                    view.skipped += count;
                    self.search_skipped = view.skipped;
                }
                SearchUpdate::Finished(completion) => finished = Some(completion),
                SearchUpdate::Failed(error) => {
                    self.status = error;
                    if let Some(view) = &mut self.search_results {
                        view.incomplete = true;
                    }
                    finished = Some(Default::default());
                }
            }
        }
        if let Some(view) = &mut self.search_results {
            if hits_changed {
                view.selected = selected_path
                    .as_ref()
                    .and_then(|path| view.results.iter().position(|hit| &hit.entry.path == path))
                    .unwrap_or_else(|| view.selected.min(view.results.len().saturating_sub(1)));
                view.selected_path = view
                    .results
                    .get(view.selected)
                    .map(|hit| hit.entry.path.clone());
            }
        }
        if disconnected && finished.is_none() {
            self.search.take();
            if self.search_cancelling {
                self.restore_previous_search_results();
                self.mode = self.search_return.mode();
                self.search_cancelling = false;
                self.set_notice("Search cancelled");
            } else {
                if let Some(view) = &mut self.search_results {
                    view.incomplete = true;
                }
                self.mode = AppMode::SearchResults;
                self.search_cancelling = false;
                self.status = "Search worker stopped unexpectedly".into();
            }
            return true;
        }
        let Some(completion) = finished else {
            return changed;
        };
        self.search.take();
        self.search_cancelling = false;
        if completion.cancelled {
            self.restore_previous_search_results();
            self.mode = self.search_return.mode();
            self.set_notice("Search cancelled");
        } else {
            self.previous_search_results = None;
            if let Some(view) = &mut self.search_results {
                view.truncated |= completion.truncated;
                view.incomplete |= completion.incomplete;
            }
            self.mode = AppMode::SearchResults;
            if self.search_matches == 0 {
                self.set_notice("No search matches found");
            }
        }
        true
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        focused_entry_from(&self.entries, self.cursor)
    }

    pub fn sort_label(&self) -> &'static str {
        match self.config.ui.sort {
            SortSetting::Name => "Name",
            SortSetting::Extension => "Extension",
            SortSetting::Size => "Size",
            SortSetting::Modified => "Modified",
            SortSetting::Type => "Type",
            SortSetting::Permissions => "Permissions",
        }
    }
}

fn insert_search_hit(results: &mut Vec<SearchHit>, hit: SearchHit) {
    let insertion = results.binary_search(&hit).unwrap_or_else(|index| index);
    results.insert(insertion, hit);
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

fn smart_report_scroll_limit(body: &str) -> u16 {
    const VISIBLE_REPORT_LINES: usize = 17;
    body.lines()
        .count()
        .saturating_sub(VISIBLE_REPORT_LINES)
        .min(u16::MAX as usize) as u16
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

fn suggested_archive_name(sources: &[PathBuf], format: ArchiveFormat) -> String {
    let base = if sources.len() == 1 {
        sources[0]
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("archive")
    } else {
        "archive"
    };
    format.append_extension(base)
}

fn validate_archive_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || path.is_absolute()
        || path.components().count() != 1
        || path.file_name().is_none()
    {
        return Err("Archive name must be a single filename".into());
    }
    Ok(())
}

fn command_available(command: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

pub fn focused_entry_from(entries: &[FileEntry], cursor: usize) -> Option<&FileEntry> {
    entries.get(cursor)
}

pub fn target_paths_from(entries: &[FileEntry], cursor: usize) -> Vec<PathBuf> {
    let marked = entries
        .iter()
        .filter(|entry| entry.selected)
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    if marked.is_empty() {
        focused_entry_from(entries, cursor)
            .map(|entry| vec![entry.path.clone()])
            .unwrap_or_default()
    } else {
        marked
    }
}

pub fn focused_entry_from_hits(entries: &[SearchHit], cursor: usize) -> Option<&FileEntry> {
    entries.get(cursor).map(|hit| &hit.entry)
}

pub fn target_paths_from_hits(entries: &[SearchHit], cursor: usize) -> Vec<PathBuf> {
    let marked = entries
        .iter()
        .filter(|hit| hit.entry.selected)
        .map(|hit| hit.entry.path.clone())
        .collect::<Vec<_>>();
    if marked.is_empty() {
        focused_entry_from_hits(entries, cursor)
            .map(|entry| vec![entry.path.clone()])
            .unwrap_or_default()
    } else {
        marked
    }
}

#[cfg(test)]
mod tests;
