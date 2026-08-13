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
pub enum BuiltinApp {
    DeviceManager,
    NetworkShares,
}

impl BuiltinApp {
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
pub struct AppsView {
    pub selected: usize,
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
    },
    EncryptionFilesystem {
        selected: usize,
        whole_disk: bool,
    },
    EncryptionPassphrase {
        filesystem: Filesystem,
        whole_disk: bool,
        label: Option<String>,
        passphrase: SecretInput,
        confirmation: SecretInput,
        confirming: bool,
        error: Option<String>,
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
    Apps(AppsView),
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
    partition_return_to_apps: bool,
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
            partition_return_to_apps: false,
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
            AppMode::Apps(view) => self.handle_apps_key(view, key),
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

    pub fn poll_archive(&mut self) -> bool {
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

    fn restore_previous_search_results(&mut self) {
        if let Some(previous) = self.previous_search_results.take() {
            self.search_results = Some(previous);
        }
        self.search_matches = self
            .search_results
            .as_ref()
            .map_or(0, |view| view.results.len());
        self.search_skipped = self.search_results.as_ref().map_or(0, |view| view.skipped);
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

    pub fn poll_network(&mut self) -> bool {
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

    pub fn poll_partitions(&mut self) -> bool {
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

    fn start_partition_refresh(&mut self, selected_path: Option<PathBuf>) {
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

    pub fn poll_partition_operation(&mut self) -> bool {
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

    fn start_partition_operation(
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
        self.search.is_some()
            || self.browser_loading
            || self.partition_refreshing
            || matches!(self.mode, AppMode::UpdateProgress)
            || matches!(self.mode, AppMode::NetworkProgress)
            || (matches!(self.mode, AppMode::Progress)
                && self.progress.total_items == 0
                && self.progress.total_bytes == 0)
    }

    pub fn search_running(&self) -> bool {
        self.search.is_some()
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        focused_entry_from(&self.entries, self.cursor)
    }

    fn focused_search_entry(&self) -> Option<&FileEntry> {
        self.search_results
            .as_ref()
            .and_then(|view| focused_entry_from_hits(&view.results, view.selected))
    }

    fn search_target_paths(&self) -> Vec<PathBuf> {
        self.search_results
            .as_ref()
            .map(|view| target_paths_from_hits(&view.results, view.selected))
            .unwrap_or_default()
    }

    fn revalidated_search_entries(
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

    fn revalidated_search_targets(&mut self) -> Vec<PathBuf> {
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

    fn revalidated_search_entry(&mut self) -> Option<FileEntry> {
        let snapshot = self.focused_search_entry()?.clone();
        self.revalidated_search_entries(vec![(snapshot.path, snapshot.kind)])
            .into_iter()
            .next()
    }

    fn activate_search_entry(&mut self, editor: bool) -> AppMode {
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

    pub fn poll_file_launch(&mut self) -> bool {
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

    fn handle_browser_key(&mut self, key: KeyEvent) -> AppMode {
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
            return AppMode::Apps(AppsView { selected: 0 });
        } else if hotkeys.info.matches(key) {
            self.modal_return = ReturnDestination::Browser;
            return AppMode::Info(self.selected_entry().cloned());
        } else if hotkeys.help.matches(key) {
            return AppMode::Help;
        } else if hotkeys.open.matches(key) || hotkeys.edit.matches(key) {
            return self.open_external(hotkeys.edit.matches(key));
        } else if hotkeys.devices.matches(key) {
            return self.open_partitions(false);
        } else if hotkeys.network_shares.matches(key) {
            return self.open_network();
        }
        AppMode::Browser
    }

    fn handle_apps_key(&mut self, mut view: AppsView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        if key.code == KeyCode::Esc || hotkeys.quit.matches(key) || hotkeys.tools.matches(key) {
            AppMode::Browser
        } else if key.code == KeyCode::Down || hotkeys.down.matches(key) {
            view.selected = (view.selected + 1).min(BuiltinApp::ALL.len() - 1);
            AppMode::Apps(view)
        } else if key.code == KeyCode::Up || hotkeys.up.matches(key) {
            view.selected = view.selected.saturating_sub(1);
            AppMode::Apps(view)
        } else if key.code == KeyCode::Enter
            || key.code == KeyCode::Right
            || hotkeys.expand.matches(key)
        {
            match BuiltinApp::ALL.get(view.selected).copied() {
                Some(BuiltinApp::DeviceManager) => self.open_partitions(true),
                Some(BuiltinApp::NetworkShares) => self.open_network(),
                None => AppMode::Apps(view),
            }
        } else {
            AppMode::Apps(view)
        }
    }

    fn handle_archive_key(&mut self, mut view: ArchiveView, key: KeyEvent) -> AppMode {
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

    fn handle_prompt_key(&mut self, mut prompt: Prompt, key: KeyEvent) -> AppMode {
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
            Prompt::ConfirmOverwrite { sources, cut } => match key.code {
                KeyCode::Enter => {
                    self.start_copy(sources.clone(), *cut, true);
                    return AppMode::Progress;
                }
                _ if hotkeys.overwrite.matches(key) => {
                    self.start_copy(sources.clone(), *cut, true);
                    return AppMode::Progress;
                }
                _ if hotkeys.skip.matches(key) => {
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

    fn handle_progress_key(&mut self, key: KeyEvent) -> AppMode {
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

    fn handle_search_form_key(&mut self, mut form: SearchForm, key: KeyEvent) -> AppMode {
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

    fn submit_search(&mut self, mut form: SearchForm) -> AppMode {
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

    fn handle_search_progress_key(&mut self, key: KeyEvent) -> AppMode {
        if key.code == KeyCode::Esc {
            if let Some(search) = &self.search {
                search.cancel.store(true, Ordering::Relaxed);
                self.search_cancelling = true;
            }
        }
        AppMode::SearchProgress
    }

    fn handle_search_results_key(&mut self, key: KeyEvent) -> AppMode {
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

    fn handle_trash_key(&mut self, mut view: TrashView, key: KeyEvent) -> AppMode {
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

    fn handle_device_key(&mut self, mut view: DeviceView, key: KeyEvent) -> AppMode {
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

    fn device_action(&mut self, view: DeviceView) -> AppMode {
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

    fn handle_network_key(&mut self, mut view: NetworkView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match key.code {
            KeyCode::Esc => AppMode::Browser,
            _ if hotkeys.quit.matches(key) || hotkeys.network_shares.matches(key) => {
                AppMode::Browser
            }
            KeyCode::Down => {
                if !view.shares.is_empty() {
                    view.selected = (view.selected + 1).min(view.shares.len() - 1);
                }
                AppMode::Network(view)
            }
            _ if hotkeys.down.matches(key) => {
                if !view.shares.is_empty() {
                    view.selected = (view.selected + 1).min(view.shares.len() - 1);
                }
                AppMode::Network(view)
            }
            KeyCode::Up => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Network(view)
            }
            _ if hotkeys.up.matches(key) => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Network(view)
            }
            _ if hotkeys.refresh.matches(key) => {
                let selected_uri = view
                    .shares
                    .get(view.selected)
                    .map(|share| share.address.uri.clone());
                self.start_network_refresh(selected_uri);
                AppMode::Network(view)
            }
            _ if hotkeys.network_add.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: network connections are disabled");
                    return AppMode::Network(view);
                }
                AppMode::Prompt(Prompt::SmbAddress {
                    input: "smb://".into(),
                    cursor: 6,
                    error: None,
                })
            }
            KeyCode::Enter | KeyCode::Right => self.network_open(view),
            _ if hotkeys.expand.matches(key) => self.network_open(view),
            _ if hotkeys.network_disconnect.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: network disconnection is disabled");
                    return AppMode::Network(view);
                }
                match view.shares.get(view.selected).cloned() {
                    Some(share) if share.mount_path.is_some() => {
                        AppMode::Prompt(Prompt::ConfirmSmbDisconnect { share })
                    }
                    _ => AppMode::Network(view),
                }
            }
            _ if hotkeys.network_forget.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: remembered shares cannot be changed");
                    return AppMode::Network(view);
                }
                match view.shares.get(view.selected).cloned() {
                    Some(share) if share.saved => {
                        AppMode::Prompt(Prompt::ConfirmSmbForget { share })
                    }
                    _ => AppMode::Network(view),
                }
            }
            _ => AppMode::Network(view),
        }
    }

    fn network_open(&mut self, view: NetworkView) -> AppMode {
        let Some(share) = view.shares.get(view.selected).cloned() else {
            return AppMode::Network(view);
        };
        if let Some(path) = share.mount_path.filter(|path| path.is_dir()) {
            self.go_to(path);
            return AppMode::Browser;
        }
        if self.config.behavior.read_only {
            self.set_notice("Read-only mode: network connections are disabled");
            return AppMode::Network(view);
        }
        if share.saved {
            let Some(username) = share.username.clone() else {
                return AppMode::Prompt(Prompt::SmbUsername {
                    address: share.address,
                    input: String::new(),
                    cursor: 0,
                    error: None,
                });
            };
            self.start_network_action(NetworkAction::Connect(ConnectRequest {
                address: share.address,
                auth: NetworkAuth::Saved {
                    username,
                    domain: share.domain.unwrap_or_default(),
                },
            }));
            AppMode::NetworkProgress
        } else {
            AppMode::Prompt(Prompt::SmbUsername {
                address: share.address,
                input: String::new(),
                cursor: 0,
                error: None,
            })
        }
    }

    fn handle_partition_key(&mut self, mut view: PartitionView, key: KeyEvent) -> AppMode {
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

    fn handle_partition_overlay(
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

    fn authorize_partition_operation(
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

    fn begin_partition_task(&mut self, mut view: PartitionView, task: PartitionTask) -> AppMode {
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

    fn partition_unmount_preflight(
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

    pub fn partition_tasks_for_view(&self, view: &PartitionView) -> Vec<PartitionTask> {
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

    pub fn partition_task_name(&self, view: &PartitionView, task: PartitionTask) -> &'static str {
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

    pub fn partition_task_description(
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

    pub fn partition_task_unavailable(
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

    fn partition_task_input(&self, _view: &PartitionView, task: PartitionTask) -> (String, String) {
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

    fn partition_action_from_input(
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

    fn partition_parent_context(
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

    fn partition_type_id(
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

    fn partition_format_action(
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

    fn partition_encryption_action(
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

    fn partition_create_action_for_region(
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

    fn partition_resize_action(
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

    fn partition_disk_layout_action(
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

    fn handle_readonly_popup(&mut self, key: KeyEvent, mode: AppMode) -> AppMode {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
            || self.config.hotkeys.quit.matches(key)
        {
            AppMode::Browser
        } else {
            mode
        }
    }

    fn handle_config_error(&mut self, path: PathBuf, error: String, key: KeyEvent) -> AppMode {
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

    fn prepare_archive(&mut self) -> AppMode {
        self.prepare_archive_paths(self.selected_paths())
    }

    fn prepare_archive_paths(&mut self, paths: Vec<PathBuf>) -> AppMode {
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

    fn start_archive(&mut self, request: ArchiveRequest) {
        self.progress = ProgressState {
            cancellable: true,
            ..ProgressState::default()
        };
        self.archive_operation = Some(archive::spawn(request));
    }

    fn set_clipboard(&mut self, mode: ClipboardMode) {
        self.set_clipboard_paths(mode, self.selected_paths());
    }

    fn set_clipboard_paths(&mut self, mode: ClipboardMode, paths: Vec<PathBuf>) {
        let Some(paths) = self.mutation_targets_from(paths) else {
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

    fn start_trash(&mut self, paths: Vec<PathBuf>) {
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

    fn restore_trash_entries(&mut self, entries: &[TrashEntry], manager: &TrashManager) -> AppMode {
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

    #[allow(dead_code)]
    fn open_devices(&mut self) -> AppMode {
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

    fn open_network(&mut self) -> AppMode {
        if !self.network_environment.samba_tools_available() {
            return AppMode::Prompt(Prompt::SmbMessage {
                title: "Network shares unavailable".into(),
                body: "Network Shares cannot start because gio or the GVFS Samba backend is unavailable. Install the required desktop integration, then try again.".into(),
                return_to_network: false,
            });
        }
        self.start_network_refresh(None);
        AppMode::Network(NetworkView {
            shares: Vec::new(),
            selected: 0,
        })
    }

    fn start_network_refresh(&mut self, selected_uri: Option<String>) {
        if self.network_refresh.is_some() {
            return;
        }
        let environment = self.network_environment.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || match network::discover_local(&environment) {
            Ok(shares) => {
                if sender
                    .send(NetworkRefreshUpdate::Snapshot {
                        result: Ok(shares),
                        finished: false,
                        secret_storage: None,
                    })
                    .is_err()
                {
                    return;
                }
                let secret_storage = network::secret_service_available(&environment);
                if sender
                    .send(NetworkRefreshUpdate::Snapshot {
                        result: network::discover_local(&environment),
                        finished: false,
                        secret_storage: Some(secret_storage),
                    })
                    .is_err()
                {
                    return;
                }
                let _ = sender.send(NetworkRefreshUpdate::Snapshot {
                    result: network::discover(&environment),
                    finished: true,
                    secret_storage: None,
                });
            }
            Err(error) => {
                let _ = sender.send(NetworkRefreshUpdate::Snapshot {
                    result: Err(error),
                    finished: true,
                    secret_storage: None,
                });
            }
        });
        self.network_refreshing = true;
        self.network_refresh = Some(RunningNetworkRefresh {
            receiver,
            selected_uri,
        });
    }

    fn start_network_action(&mut self, action: NetworkAction) {
        if self.network_operation.is_some() {
            return;
        }
        let environment = self.network_environment.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(network::perform(action, &environment));
        });
        self.network_operation = Some(RunningNetworkOperation { receiver });
    }

    fn open_partitions(&mut self, return_to_apps: bool) -> AppMode {
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

    fn reopen_partitions(&mut self) -> AppMode {
        self.open_partitions(self.partition_return_to_apps)
    }

    pub fn partition_returns_to_apps(&self) -> bool {
        self.partition_return_to_apps
    }

    pub fn device_manager_available(&self) -> bool {
        command_available("lsblk")
    }

    pub fn network_shares_available(&self) -> bool {
        self.network_environment.samba_tools_available()
    }

    fn open_external(&mut self, editor: bool) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        self.open_external_entry(&entry, editor, ReturnDestination::Browser)
    }

    fn open_external_entry(
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
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs::File,
        os::unix::ffi::OsStringExt,
        os::unix::fs::PermissionsExt,
        sync::{atomic::AtomicBool, Arc},
    };

    fn wait_for_search(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.search.is_some() && Instant::now() < deadline {
            if !app.poll_search() {
                thread::yield_now();
            }
        }
        assert!(app.search.is_none(), "search worker timed out");
    }

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

    fn key(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
    }

    #[test]
    fn browser_and_search_result_selection_share_mark_then_cursor_semantics() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ["one.txt", "two.txt", "three.txt"].map(|name| temp.path().join(name));
        for path in &paths {
            std::fs::write(path, b"").unwrap();
        }
        let mut entries = paths
            .iter()
            .map(|path| search::hit_for_test(path.clone(), "txt").entry)
            .collect::<Vec<_>>();
        entries[0].selected = true;
        entries[1].selected = true;
        let mut hits = entries
            .iter()
            .cloned()
            .map(|entry| search::hit_for_test(entry.path, "txt"))
            .collect::<Vec<_>>();
        hits[0].entry.selected = true;
        hits[1].entry.selected = true;

        assert_eq!(target_paths_from(&entries, 2), paths[..2]);
        assert_eq!(target_paths_from_hits(&hits, 2), paths[..2]);
        entries.iter_mut().for_each(|entry| entry.selected = false);
        hits.iter_mut().for_each(|hit| hit.entry.selected = false);
        assert_eq!(target_paths_from(&entries, 2), vec![paths[2].clone()]);
        assert_eq!(target_paths_from_hits(&hits, 2), vec![paths[2].clone()]);
        assert!(std::ptr::eq(
            focused_entry_from(&entries, 2).unwrap(),
            &entries[2]
        ));
    }

    fn install_search_results(app: &mut App, paths: &[PathBuf]) {
        let mut draft = SearchDraft::quick(app.current_dir.clone());
        draft.name = "txt".into();
        app.search_results = Some(SearchView {
            request: draft.compile(true).unwrap(),
            results: paths
                .iter()
                .map(|path| search::hit_for_test(path.clone(), "txt"))
                .collect(),
            selected: 0,
            selected_path: paths.first().cloned(),
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchResults;
    }

    #[test]
    fn search_results_select_copy_and_unavailable_creation_use_result_state() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, b"").unwrap();
        std::fs::write(&second, b"").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &[first.clone(), second.clone()]);

        app.handle_key(key(' '));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(key(' '));
        app.handle_key(key('c'));
        assert_eq!(app.clipboard.as_ref().unwrap().paths, vec![first, second]);
        assert!(matches!(app.mode, AppMode::SearchResults));

        app.handle_key(key('n'));
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert_eq!(app.status, "Unavailable in search results");
    }

    #[test]
    fn search_result_copy_revalidates_mixed_marked_targets() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("valid.txt");
        let stale = temp.path().join("stale.txt");
        std::fs::write(&valid, b"updated contents").unwrap();
        std::fs::write(&stale, b"stale").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &[valid.clone(), stale.clone()]);
        let view = app.search_results.as_mut().unwrap();
        view.results[0].entry.selected = true;
        view.results[1].entry.selected = true;
        view.selected = 1;
        view.selected_path = Some(stale.clone());
        std::fs::remove_file(&stale).unwrap();

        app.handle_key(key('c'));

        assert_eq!(app.clipboard.as_ref().unwrap().paths, vec![valid.clone()]);
        let view = app.search_results.as_ref().unwrap();
        assert_eq!(view.results.len(), 1);
        assert_eq!(view.results[0].entry.path, valid);
        assert!(view.results[0].entry.selected);
        assert_eq!(view.results[0].entry.size, b"updated contents".len() as u64);
        assert_eq!(view.selected, 0);
        assert!(app.visible_status().contains("no longer available"));
    }

    #[test]
    fn search_result_cut_revalidates_mixed_marked_targets() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("valid.txt");
        let stale = temp.path().join("stale.txt");
        std::fs::write(&valid, b"valid").unwrap();
        std::fs::write(&stale, b"stale").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &[valid.clone(), stale.clone()]);
        for hit in &mut app.search_results.as_mut().unwrap().results {
            hit.entry.selected = true;
        }
        std::fs::remove_file(stale).unwrap();

        app.handle_key(key('x'));

        let clipboard = app.clipboard.as_ref().unwrap();
        assert!(matches!(clipboard.mode, ClipboardMode::Cut));
        assert_eq!(clipboard.paths, vec![valid]);
        assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);
    }

    #[test]
    fn search_result_info_revalidates_current_metadata_and_removes_stale_entry() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current.txt");
        std::fs::write(&current, b"old").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&current));
        std::fs::write(&current, b"new metadata").unwrap();

        app.handle_key(key('I'));

        assert!(matches!(
            &app.mode,
            AppMode::Info(Some(entry)) if entry.path == current && entry.size == b"new metadata".len() as u64
        ));
        app.mode = AppMode::SearchResults;
        std::fs::remove_file(&current).unwrap();
        app.handle_key(key('I'));
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert!(app.visible_status().contains("no longer available"));
    }

    #[test]
    fn search_result_rename_returns_to_results_and_refreshes_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old.txt");
        let new = temp.path().join("new.txt");
        std::fs::write(&old, b"old").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&old));

        app.handle_key(key('r'));
        assert!(matches!(app.mode, AppMode::Prompt(Prompt::Rename { .. })));
        if let AppMode::Prompt(Prompt::Rename { input, cursor, .. }) = &mut app.mode {
            *input = "new.txt".into();
            *cursor = input.len();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert_eq!(
            app.search_results.as_ref().unwrap().results[0].entry.path,
            new
        );
        assert!(!old.exists());
    }

    #[test]
    fn stale_search_result_is_removed_before_rename() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gone.txt");
        std::fs::write(&path, b"").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&path));
        std::fs::remove_file(path).unwrap();

        app.handle_key(key('r'));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert_eq!(app.status, "Search result is no longer available");
    }

    #[test]
    fn stale_result_activation_revalidates_before_using_snapshot_kind() {
        let temp = tempfile::tempdir().unwrap();
        let deleted_file = temp.path().join("deleted.txt");
        let replaced_directory = temp.path().join("was-directory.txt");
        std::fs::write(&deleted_file, b"").unwrap();
        std::fs::create_dir(&replaced_directory).unwrap();
        let mut app = test_app(temp.path());
        install_search_results(
            &mut app,
            &[deleted_file.clone(), replaced_directory.clone()],
        );
        std::fs::remove_file(&deleted_file).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);

        std::fs::remove_dir(&replaced_directory).unwrap();
        std::fs::write(&replaced_directory, b"now a file").unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert!(app.status.contains("changed type"));
    }

    #[test]
    fn stale_archive_result_is_not_reinterpreted_as_a_creation_source() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("old.txt.zip");
        std::fs::write(&archive, b"not needed").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&archive));
        std::fs::remove_file(&archive).unwrap();

        app.handle_key(key('z'));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert_eq!(app.status, "Search result is no longer available");
    }

    #[test]
    fn archive_result_revalidation_skips_changed_type_but_keeps_valid_marked_sources() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("valid.txt");
        let changed = temp.path().join("changed.txt");
        std::fs::write(&valid, b"valid").unwrap();
        std::fs::write(&changed, b"old").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &[valid.clone(), changed.clone()]);
        for hit in &mut app.search_results.as_mut().unwrap().results {
            hit.entry.selected = true;
        }
        std::fs::remove_file(&changed).unwrap();
        std::fs::create_dir(&changed).unwrap();

        app.handle_key(key('z'));

        assert!(
            matches!(app.mode, AppMode::Prompt(Prompt::ArchiveFormat { ref sources, .. }) if sources == &vec![valid])
        );
        assert_eq!(app.search_results.as_ref().unwrap().results.len(), 1);
        assert!(app.status.contains("changed type"));
    }

    #[test]
    fn archive_result_revalidation_does_not_follow_or_reclassify_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let link = temp.path().join("linked.txt");
        std::fs::write(&target, b"target").unwrap();
        symlink(&target, &link).unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&link));

        app.handle_key(key('z'));

        assert!(
            matches!(app.mode, AppMode::Prompt(Prompt::ArchiveFormat { ref sources, .. }) if sources == &vec![link])
        );
    }

    #[test]
    fn missing_search_result_is_removed_before_trash_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.txt");
        std::fs::write(&missing, b"stale").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&missing));
        std::fs::remove_file(&missing).unwrap();

        app.handle_key(key('d'));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert_eq!(app.status, "Search result is no longer available");
    }

    #[test]
    fn file_replaced_by_directory_is_removed_before_trash_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let replaced = temp.path().join("replaced.txt");
        std::fs::write(&replaced, b"file snapshot").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&replaced));
        std::fs::remove_file(&replaced).unwrap();
        std::fs::create_dir(&replaced).unwrap();

        app.handle_key(key('d'));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(replaced.is_dir());
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert_eq!(
            app.status,
            "Search result changed type and is no longer available"
        );
    }

    #[test]
    fn directory_replaced_by_file_is_removed_before_quick_trash() {
        let temp = tempfile::tempdir().unwrap();
        let replaced = temp.path().join("replaced.txt");
        std::fs::create_dir(&replaced).unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&replaced));
        std::fs::remove_dir(&replaced).unwrap();
        std::fs::write(&replaced, b"replacement file").unwrap();

        app.handle_key(key('D'));

        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(replaced.is_file());
        assert!(app.operation.is_none());
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert_eq!(
            app.status,
            "Search result changed type and is no longer available"
        );
    }

    #[test]
    fn trash_confirmation_keeps_only_valid_marked_search_results() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("valid.txt");
        let missing = temp.path().join("missing.txt");
        let changed = temp.path().join("changed.txt");
        std::fs::write(&valid, b"valid").unwrap();
        std::fs::write(&missing, b"missing").unwrap();
        std::fs::write(&changed, b"file snapshot").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &[valid.clone(), missing.clone(), changed.clone()]);
        for hit in &mut app.search_results.as_mut().unwrap().results {
            hit.entry.selected = true;
        }
        std::fs::remove_file(&missing).unwrap();
        std::fs::remove_file(&changed).unwrap();
        std::fs::create_dir(&changed).unwrap();

        app.handle_key(key('d'));

        assert!(
            matches!(&app.mode, AppMode::Prompt(Prompt::ConfirmTrash { paths }) if paths == &vec![valid.clone()])
        );
        let view = app.search_results.as_ref().unwrap();
        assert_eq!(view.results.len(), 1);
        assert_eq!(view.results[0].entry.path, valid);
        assert!(view.results[0].entry.selected);
        assert!(changed.is_dir());
        assert!(app.status.contains("changed type"));
    }

    #[test]
    fn refresh_keeps_mark_and_moves_cursor_to_nearest_remaining_result() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ["one.txt", "two.txt", "three.txt"].map(|name| temp.path().join(name));
        for path in &paths {
            std::fs::write(path, b"").unwrap();
        }
        let mut app = test_app(temp.path());
        install_search_results(&mut app, &paths);
        {
            let view = app.search_results.as_mut().unwrap();
            view.selected = 1;
            view.results[2].entry.selected = true;
        }
        std::fs::remove_file(&paths[1]).unwrap();

        app.refresh_search_results(None);

        let view = app.search_results.as_ref().unwrap();
        assert_eq!(view.selected, 1);
        assert_eq!(view.results[1].entry.path, paths[2]);
        assert!(view.results[1].entry.selected);
    }

    #[test]
    fn cut_result_pasted_from_browser_refreshes_retained_results_after_move() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"move me").unwrap();
        std::fs::create_dir(&destination).unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&source));
        app.handle_key(key('x'));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.go_to(destination.clone());

        app.handle_key(key('p'));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.operation.is_some() && Instant::now() < deadline {
            if !app.poll_operation() {
                thread::yield_now();
            }
        }

        assert!(destination.join("source.txt").is_file());
        assert!(app.search_results.as_ref().unwrap().results.is_empty());
        assert!(matches!(app.mode, AppMode::Browser));
    }

    #[test]
    fn read_only_result_mutations_stay_in_results() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        std::fs::write(&source, b"").unwrap();
        let mut app = test_app(temp.path());
        app.config.behavior.read_only = true;
        install_search_results(&mut app, std::slice::from_ref(&source));

        for key_code in ['x', 'c', 'd', 'D', 'z'] {
            app.handle_key(key(key_code));
            assert!(matches!(app.mode, AppMode::SearchResults));
        }
        assert!(source.exists());
        assert!(app.clipboard.is_none());
    }

    #[test]
    fn search_result_terminal_editor_and_launch_error_return_to_results() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        std::fs::write(&path, b"notes").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&path));
        app.config.open.editor = "vim".into();

        app.handle_key(key('e'));
        let action = app.take_terminal_editor().unwrap();
        assert_eq!(action.return_to, ReturnDestination::SearchResults);
        app.finish_terminal_editor(&action, Ok(()));
        assert!(matches!(app.mode, AppMode::SearchResults));

        app.launch_sender
            .try_send(PendingLaunchError {
                error: LaunchError {
                    program: "opener".into(),
                    path,
                    detail: "failed".into(),
                },
                return_to: ReturnDestination::SearchResults,
            })
            .unwrap();
        assert!(app.poll_file_launch());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
    }

    #[test]
    fn overlapping_launcher_errors_keep_their_individual_return_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        for (detail, return_to) in [
            ("browser error", ReturnDestination::Browser),
            ("results error", ReturnDestination::SearchResults),
        ] {
            app.launch_sender
                .try_send(PendingLaunchError {
                    error: LaunchError {
                        program: "opener".into(),
                        path: temp.path().join("file.txt"),
                        detail: detail.into(),
                    },
                    return_to,
                })
                .unwrap();
        }
        app.mode = AppMode::SearchResults;

        assert!(app.poll_file_launch());
        assert!(
            matches!(app.mode, AppMode::Prompt(Prompt::OpenError { ref body, .. }) if body.contains("browser error"))
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
        assert!(app.poll_file_launch());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
    }

    #[test]
    fn search_result_info_closes_back_to_results_with_context() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        std::fs::write(&path, b"notes").unwrap();
        let mut app = test_app(temp.path());
        install_search_results(&mut app, std::slice::from_ref(&path));

        app.handle_key(key('I'));
        assert!(matches!(app.mode, AppMode::Info(Some(ref entry)) if entry.path == path));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
    }

    fn test_network_environment(root: &Path) -> NetworkEnvironment {
        let gio = root.join("gio");
        std::fs::write(
            &gio,
            "#!/bin/sh\nif [ \"$1\" = list ]; then exit 0; fi\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&gio).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&gio, permissions).unwrap();
        NetworkEnvironment {
            gio,
            secret_tool: None,
            runtime_dir: root.join("runtime"),
            shares_file: root.join("network-shares.toml"),
        }
    }

    fn wait_for_archive(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.archive_operation.is_some() && Instant::now() < deadline {
            if !app.poll_archive() {
                thread::yield_now();
            }
        }
        assert!(app.archive_operation.is_none(), "archive worker timed out");
    }

    #[test]
    fn archive_hotkey_creates_and_inspects_an_archive_without_external_tools() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        std::fs::write(&source, b"archive me").unwrap();
        let mut app = test_app(temp.path());
        app.cursor = app
            .entries
            .iter()
            .position(|entry| entry.path == source)
            .unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::ArchiveFormat { selected: 0, .. })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::ArchiveName {
                format: ArchiveFormat::TarGz,
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Progress));
        wait_for_archive(&mut app);

        let archive = temp.path().join("source.txt.tar.gz");
        assert!(archive.is_file());
        app.cursor = app
            .entries
            .iter()
            .position(|entry| entry.path == archive)
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::ArchiveActions { selected: 0, .. })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_archive(&mut app);
        assert!(matches!(
            app.mode,
            AppMode::Archive(ArchiveView { ref entries, .. })
                if entries.iter().any(|entry| entry.path == Path::new("source.txt"))
        ));
    }

    #[test]
    fn successful_multi_selection_archive_clears_marked_sources() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let mut app = test_app(temp.path());
        for entry in &mut app.entries {
            entry.selected = entry.path == first || entry.path == second;
        }

        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_archive(&mut app);

        let archive = temp.path().join("archive.tar.gz");
        assert!(archive.is_file());
        assert!(app.entries.iter().all(|entry| !entry.selected));
        assert_eq!(
            app.selected_entry().map(|entry| entry.path.as_path()),
            Some(archive.as_path())
        );
    }

    #[test]
    fn archive_actions_respect_read_only_mode_but_allow_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("bundle.tar");
        let file = File::create(&archive).unwrap();
        tar::Builder::new(file).finish().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            true,
        );
        app.cursor = app
            .entries
            .iter()
            .position(|entry| entry.path == archive)
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
        assert_eq!(app.status, "Read-only mode: archive extraction is disabled");

        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_archive(&mut app);
        assert!(matches!(app.mode, AppMode::Archive(_)));
    }

    #[test]
    fn archive_progress_escape_requests_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        app.archive_operation = Some(RunningArchive {
            receiver,
            cancel: Arc::clone(&cancel),
        });
        app.progress.cancellable = true;
        app.mode = AppMode::Progress;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.progress.cancelling);
        drop(sender);
    }

    #[test]
    fn archive_names_are_single_safe_filenames() {
        assert!(validate_archive_name("backup.tar.gz").is_ok());
        assert!(validate_archive_name("").is_err());
        assert!(validate_archive_name("../backup.zip").is_err());
        assert!(validate_archive_name("folder/backup.zip").is_err());
        assert_eq!(
            suggested_archive_name(&[PathBuf::from("photos")], ArchiveFormat::Zip),
            "photos.zip"
        );
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

        for ch in ['d', 'D', 'x', 'c', 'p', 'z', 'r', 'm', 'v', 'q'] {
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
            .try_send(PendingLaunchError {
                error: LaunchError {
                    program: "missing-opener".into(),
                    path: temp.path().join("example.txt"),
                    detail: "application not found".into(),
                },
                return_to: ReturnDestination::Browser,
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
            .try_send(PendingLaunchError {
                error: LaunchError {
                    program: "missing-editor".into(),
                    path: path.clone(),
                    detail: "application not found".into(),
                },
                return_to: ReturnDestination::Browser,
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
                kind: "part".into(),
                filesystem: Some("crypto_LUKS".into()),
                encrypted: true,
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
                kind: "part".into(),
                filesystem: Some("crypto_LUKS".into()),
                encrypted: true,
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
                kind: "part".into(),
                filesystem: Some("crypto_LUKS".into()),
                encrypted: true,
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
    fn slash_opens_quick_search_and_empty_f_expands_it() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('/'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if !form.advanced));
        app.handle_key(key('F'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.advanced && form.draft.scope == SearchScope::CurrentDirectory));
    }

    #[test]
    fn advanced_search_up_down_selects_sections_and_tab_stays_within_section() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 0));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 1));
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Match && form.field == 0));
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.section == SearchSection::Scope));
    }

    #[test]
    fn advanced_search_cycles_choices_and_toggles_entry_kinds() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.scope == SearchScope::CurrentDirectory));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.name_mode == crate::search::NameMode::Glob));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if !form.draft.types.contains(crate::entry::EntryKind::Directory)));
        for _ in 0..9 {
            app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.include_ignored_hidden));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.result_limit == crate::search::ResultLimit::TenThousand));
    }

    #[test]
    fn advanced_search_type_arrows_wrap_without_toggling() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.error = Some("keep this validation error".into());
        }

        let initial_types = match &app.mode {
            AppMode::SearchForm(form) => form.draft.types,
            _ => panic!("advanced search form was not open"),
        };
        for expected_field in [1, 2, 3, 4, 0] {
            app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            assert!(matches!(&app.mode, AppMode::SearchForm(form)
                if form.field == expected_field
                    && form.draft.types == initial_types
                    && form.error.as_deref() == Some("keep this validation error")));
        }

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.field == 4
                && form.draft.types == initial_types
                && form.error.as_deref() == Some("keep this validation error")));

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.field == 4 && form.draft.types == crate::search::EntryKinds::OTHER));
    }

    #[test]
    fn advanced_search_enter_validates_from_every_section_and_keeps_error_inline() {
        for section_moves in 0..4 {
            let mut app = test_app(tempfile::tempdir().unwrap().path());
            app.handle_key(key('F'));
            for _ in 0..section_moves {
                app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
            if let AppMode::SearchForm(form) = &mut app.mode {
                form.draft.minimum_size = "not-a-size".into();
            }
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::SearchForm(ref form)
                if form.error.as_deref().is_some_and(|error| error.contains("invalid minimum size"))));
            if let AppMode::SearchForm(form) = &mut app.mode {
                form.draft.minimum_size.clear();
                form.draft.name = "needle".into();
            }
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::SearchProgress));
            app.search
                .take()
                .unwrap()
                .cancel
                .store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn invalid_content_regex_keeps_form_and_does_not_start_or_replace_search() {
        let temp = tempfile::tempdir().unwrap();
        let old_path = temp.path().join("old-result");
        std::fs::write(&old_path, []).unwrap();
        let mut app = test_app(temp.path());
        let mut old_draft = SearchDraft::quick(temp.path().to_path_buf());
        old_draft.name = "old".into();
        app.search_results = Some(SearchView {
            request: old_draft.compile(true).unwrap(),
            results: vec![search::hit_for_test(old_path.clone(), "old")],
            selected: 0,
            selected_path: Some(old_path.clone()),
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchResults;
        app.handle_key(key('F'));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.content = "(".into();
            form.draft.content_mode = crate::search::ContentMode::Regex;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search.is_none());
        assert_eq!(
            app.search_results.as_ref().unwrap().results[0].entry.path,
            old_path
        );
        assert!(matches!(&app.mode, AppMode::SearchForm(form)
            if form.draft.content == "(" && form.error.as_deref().is_some_and(|e| e.contains("invalid content regex"))));
    }

    #[test]
    fn advanced_search_preserves_validation_error_while_navigating() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.minimum_size = "bad".into();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Up] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
        }
    }

    #[test]
    fn advanced_search_text_fields_keep_independent_unicode_cursors() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.name = "a界b".into();
            form.cursors.name = 2;
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for ch in "xy".chars() {
            app.handle_key(key(ch));
        }
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(key('界'));
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(key('Z'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.name == "a界Zb" && form.draft.content == "x界y"
                && form.cursors.name == 3 && form.cursors.content == 2));
    }

    #[test]
    fn advanced_search_space_is_noop_for_non_type_choices() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        app.handle_key(key(' '));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.draft.scope == SearchScope::Filesystem && form.draft.name.is_empty()));
    }

    #[test]
    fn configured_filesystem_binding_expands_only_an_empty_quick_query() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.config =
            toml::from_str::<crate::config::Config>("[hotkeys]\nsearch_filesystem = 'G'").unwrap();
        app.handle_key(key('/'));
        app.handle_key(key('G'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.advanced));
        app.mode = AppMode::Browser;
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(key('G'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.name == "xG"));
    }

    #[test]
    fn quick_search_error_clears_only_when_query_value_changes() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('/'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
        for code in [KeyCode::Left, KeyCode::Home, KeyCode::Up] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
        }
        app.handle_key(key('x'));
        assert!(
            matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none() && form.draft.name == "x")
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.search
            .take()
            .unwrap()
            .cancel
            .store(true, Ordering::Relaxed);

        app.mode = AppMode::Browser;
        app.handle_key(key('/'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(key('x'));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none() && form.draft.name.is_empty())
        );
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    }

    #[test]
    fn advanced_search_scope_cycles_from_exact_initial_scope() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        assert!(
            matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::Filesystem)
        );
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(
            matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::CurrentDirectory)
        );
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(
            matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.scope == SearchScope::Filesystem)
        );
    }

    #[test]
    fn advanced_search_error_survives_caret_moves_and_noop_controls() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.minimum_size = "bad".into();
            form.draft.content = "a界b".into();
            form.cursors.content = 2;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_some()));
    }

    #[test]
    fn advanced_search_error_clears_only_after_value_mutation() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('F'));
        if let AppMode::SearchForm(form) = &mut app.mode {
            form.draft.minimum_size = "bad".into();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.error.is_none()));
    }

    #[test]
    fn typed_f_is_query_input_and_global_f_uses_filesystem_scope() {
        let mut app = test_app(tempfile::tempdir().unwrap().path());
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(key('F'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form) if form.draft.name == "xF"));
        app.mode = AppMode::Browser;
        app.handle_key(key('F'));
        assert!(matches!(app.mode, AppMode::SearchForm(ref form)
            if form.advanced && form.draft.scope == SearchScope::Filesystem));
    }

    #[test]
    fn search_typing_does_not_start_worker_and_enter_starts_once() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("needle.txt"), b"needle").unwrap();
        let mut app = test_app(temp.path());

        app.handle_key(key('/'));
        for character in "needle".chars() {
            app.handle_key(key(character));
            assert!(app.search.is_none());
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search.is_some());
        let cancel = Arc::clone(&app.search.as_ref().unwrap().cancel);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(Arc::ptr_eq(&cancel, &app.search.as_ref().unwrap().cancel));
        wait_for_search(&mut app);
    }

    #[test]
    fn search_results_preserve_browser_state_and_old_results_on_form_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let target = child.join("needle.txt");
        std::fs::write(&target, b"needle").unwrap();
        let mut app = test_app(temp.path());
        app.expanded_directories.insert(child.clone());
        app.refresh();
        app.cursor = app
            .entries
            .iter()
            .position(|entry| entry.path == target)
            .unwrap();
        app.entries[app.cursor].selected = true;
        let cursor_path = app.selected_entry().unwrap().path.clone();
        let marked = app
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();

        app.handle_key(key('/'));
        for character in "needle".chars() {
            app.handle_key(key(character));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_search(&mut app);
        assert!(matches!(app.mode, AppMode::SearchResults));
        let result_count = app.search_results.as_ref().unwrap().results.len();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(app.current_dir, temp.path());
        assert_eq!(app.selected_entry().unwrap().path, cursor_path);
        assert!(app.expanded_directories.contains(&child));
        assert_eq!(
            app.entries
                .iter()
                .filter(|entry| entry.selected)
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            marked
        );

        app.mode = AppMode::SearchResults;
        app.handle_key(key('/'));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert_eq!(
            app.search_results.as_ref().unwrap().results.len(),
            result_count
        );
        assert_eq!(app.selected_entry().unwrap().path, cursor_path);
    }

    #[test]
    fn poll_search_is_bounded_to_updates_per_tick() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..UPDATES_PER_UI_TICK + 20 {
            std::fs::write(temp.path().join(format!("needle-{index}.txt")), b"").unwrap();
        }
        let mut app = test_app(temp.path());
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "needle".into();
        let request = draft.compile(true).unwrap();
        app.search_results = Some(SearchView {
            request,
            results: Vec::new(),
            selected: 0,
            selected_path: None,
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchProgress;
        let updates = (0..UPDATES_PER_UI_TICK + 20)
            .map(|index| {
                SearchUpdate::Match(search::hit_for_test(
                    temp.path().join(format!("needle-{index}.txt")),
                    "needle",
                ))
            })
            .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
            .collect();
        app.search = Some(search::running_search_for_test(updates));

        assert!(app.poll_search());
        assert_eq!(app.search_matches, UPDATES_PER_UI_TICK);
        assert!(app.search.is_some());
        assert!(matches!(app.mode, AppMode::SearchProgress));
    }

    fn poll_synthetic_hits(count: usize) -> (App, Duration) {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "item".into();
        app.search_results = Some(SearchView {
            request: draft.compile(true).unwrap(),
            results: Vec::new(),
            selected: 0,
            selected_path: None,
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        let updates = (0..count)
            .rev()
            .map(|index| {
                let name = format!("item-{index:05}-Straße");
                SearchUpdate::Match(search::synthetic_hit_for_test(
                    PathBuf::from("/benchmark").join(&name),
                    name,
                ))
            })
            .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
            .collect();
        app.search = Some(search::running_search_for_test(updates));
        app.mode = AppMode::SearchProgress;
        search::reset_case_fold_calls_for_test();
        let started = Instant::now();
        while app.search.is_some() {
            app.poll_search();
        }
        (app, started.elapsed())
    }

    #[test]
    fn poll_search_sorts_ten_thousand_streamed_hits_without_comparator_folding() {
        let (app, _) = poll_synthetic_hits(10_000);
        let results = &app.search_results.as_ref().unwrap().results;
        assert_eq!(results.len(), 10_000);
        assert!(results.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(search::case_fold_calls_for_test(), 0);
        assert_eq!(search::sort_key_allocations_for_test(), 0);
    }

    #[test]
    #[ignore]
    fn benchmark_poll_search_stream_sort_ten_thousand() {
        let mut samples = Vec::new();
        for _ in 0..9 {
            let (_, elapsed) = poll_synthetic_hits(10_000);
            assert_eq!(search::case_fold_calls_for_test(), 0);
            assert_eq!(search::sort_key_allocations_for_test(), 0);
            samples.push(elapsed.as_micros());
        }
        samples.sort_unstable();
        eprintln!(
            "PERF app_search_sort hits=10000 median_us={} comparator_case_folds=0",
            samples[samples.len() / 2]
        );
    }

    #[test]
    fn poll_search_adds_aggregate_skipped_count() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "needle".into();
        app.search_results = Some(SearchView {
            request: draft.compile(true).unwrap(),
            results: Vec::new(),
            selected: 0,
            selected_path: None,
            skipped: 2,
            truncated: false,
            incomplete: false,
        });
        app.search = Some(search::running_search_for_test(vec![
            SearchUpdate::Skipped(7),
            SearchUpdate::Finished(Default::default()),
        ]));

        assert!(app.poll_search());
        assert_eq!(app.search_skipped, 9);
        assert_eq!(app.search_results.as_ref().unwrap().skipped, 9);
    }

    #[test]
    fn cancelled_search_returns_to_results_without_clearing_them() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("needle.txt"), b"needle").unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(key('/'));
        for character in "needle".chars() {
            app.handle_key(key(character));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_search(&mut app);
        let old_count = app.search_results.as_ref().unwrap().results.len();
        app.mode = AppMode::SearchResults;
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.previous_search_results.is_some());
        app.search = Some(search::running_search_for_test(vec![
            SearchUpdate::Finished(search::SearchCompletion {
                cancelled: true,
                truncated: false,
                incomplete: false,
            }),
        ]));

        assert!(app.poll_search());
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert_eq!(
            app.search_results.as_ref().unwrap().results.len(),
            old_count
        );
    }

    #[test]
    fn streamed_hits_sort_and_keep_selected_path() {
        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().join("needle-z.txt");
        let better = temp.path().join("needle.txt");
        std::fs::write(&selected, b"").unwrap();
        std::fs::write(&better, b"").unwrap();
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "needle".into();
        let request = draft.compile(true).unwrap();
        let selected_hit = search::hit_for_test(selected.clone(), "needle");
        let better_hit = search::hit_for_test(better.clone(), "needle");
        let mut app = test_app(temp.path());
        app.search_results = Some(SearchView {
            request,
            results: vec![selected_hit],
            selected: 0,
            selected_path: Some(selected.clone()),
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchProgress;
        app.search = Some(search::running_search_for_test(vec![
            SearchUpdate::Match(better_hit),
            SearchUpdate::Finished(Default::default()),
        ]));

        assert!(app.poll_search());
        let view = app.search_results.as_ref().unwrap();
        assert_eq!(view.results[0].entry.path, better);
        assert_eq!(view.results[view.selected].entry.path, selected);
        assert_eq!(view.selected_path.as_deref(), Some(selected.as_path()));
    }

    #[test]
    fn mixed_name_hits_use_production_insertion_order_and_keep_selected_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = |bytes: &[u8], group: &str| {
            temp.path()
                .join(group)
                .join(OsString::from_vec(bytes.to_vec()))
        };
        let selected = path(b"Z", "selected");
        let paths = [
            selected.clone(),
            path(b"a", "valid"),
            path(&[0x60, 0x80], "invalid-0"),
            path(b"Alpha", "collision-0"),
            path(b"alpha", "collision-1"),
            path("Straße".as_bytes(), "unicode-0"),
            path(b"STRASSE", "unicode-1"),
            path(&[0x61, 0xff], "invalid-1"),
        ];
        let hits = paths
            .iter()
            .map(|path| search::synthetic_hit_for_test(path.clone(), "hit".into()))
            .collect::<Vec<_>>();
        let mut expected = hits.clone();
        expected.sort();

        let mut inserted = Vec::new();
        for index in [2, 6, 0, 7, 3, 1, 5, 4] {
            insert_search_hit(&mut inserted, hits[index].clone());
        }
        assert_eq!(
            inserted
                .iter()
                .map(|hit| &hit.entry.path)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|hit| &hit.entry.path)
                .collect::<Vec<_>>()
        );

        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "hit".into();
        let mut app = test_app(temp.path());
        app.search_results = Some(SearchView {
            request: draft.compile(true).unwrap(),
            results: vec![hits[0].clone()],
            selected: 0,
            selected_path: Some(selected.clone()),
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchProgress;
        app.search = Some(search::running_search_for_test(
            [2, 6, 7, 3, 1, 5, 4]
                .into_iter()
                .map(|index| SearchUpdate::Match(hits[index].clone()))
                .chain(std::iter::once(SearchUpdate::Finished(Default::default())))
                .collect(),
        ));
        assert!(app.poll_search());
        let view = app.search_results.as_ref().unwrap();
        assert_eq!(
            view.results
                .iter()
                .map(|hit| &hit.entry.path)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|hit| &hit.entry.path)
                .collect::<Vec<_>>()
        );
        assert_eq!(view.results[view.selected].entry.path, selected);
        assert_eq!(view.selected_path.as_deref(), Some(selected.as_path()));
    }

    #[test]
    fn disconnected_search_preserves_hits_and_marks_results_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("needle.txt");
        std::fs::write(&path, b"").unwrap();
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.name = "needle".into();
        let request = draft.compile(true).unwrap();
        let hit = search::hit_for_test(path.clone(), "needle");
        let mut app = test_app(temp.path());
        app.search_results = Some(SearchView {
            request,
            results: Vec::new(),
            selected: 0,
            selected_path: None,
            skipped: 0,
            truncated: false,
            incomplete: false,
        });
        app.mode = AppMode::SearchProgress;
        app.search = Some(search::running_search_for_test(vec![SearchUpdate::Match(
            hit,
        )]));

        assert!(app.poll_search());
        assert!(matches!(app.mode, AppMode::SearchResults));
        assert!(app.search.is_none());
        let view = app.search_results.as_ref().unwrap();
        assert_eq!(view.results[0].entry.path, path);
        assert!(view.incomplete);
        assert_eq!(app.status, "Search worker stopped unexpectedly");
    }

    #[test]
    fn disconnected_cancelling_search_restores_prior_browser_results() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("needle.txt"), b"").unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(key('/'));
        for character in "needle".chars() {
            app.handle_key(key(character));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_search(&mut app);
        let old_paths = app
            .search_results
            .as_ref()
            .unwrap()
            .results
            .iter()
            .map(|hit| hit.entry.path.clone())
            .collect::<Vec<_>>();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.search_cancelling = true;
        app.search = Some(search::running_search_for_test(Vec::new()));

        assert!(app.poll_search());
        assert!(matches!(app.mode, AppMode::Browser));
        assert!(app.search.is_none());
        assert_eq!(
            app.search_results
                .as_ref()
                .unwrap()
                .results
                .iter()
                .map(|hit| hit.entry.path.clone())
                .collect::<Vec<_>>(),
            old_paths
        );
    }

    #[test]
    fn browser_search_cancellation_restores_existing_results() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("needle.txt"), b"").unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(key('/'));
        for character in "needle".chars() {
            app.handle_key(key(character));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_search(&mut app);
        let old_count = app.search_results.as_ref().unwrap().results.len();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key('/'));
        app.handle_key(key('x'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.search = Some(search::running_search_for_test(vec![
            SearchUpdate::Finished(search::SearchCompletion {
                cancelled: true,
                ..Default::default()
            }),
        ]));

        assert!(app.poll_search());
        assert!(matches!(app.mode, AppMode::Browser));
        assert_eq!(
            app.search_results.as_ref().unwrap().results.len(),
            old_count
        );
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
        app.request_browser_load(Some(target.clone()));

        let deadline = Instant::now() + Duration::from_secs(5);
        while app.browser_loading && Instant::now() < deadline {
            app.poll_browser_load();
            thread::sleep(Duration::from_millis(1));
        }

        assert!(!app.browser_loading);
        assert!(app.entries.iter().any(|entry| entry.path == target));
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
    fn u_does_not_open_the_disk_manager() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
    }

    #[test]
    fn uppercase_n_opens_the_separate_network_share_manager() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.network_environment = test_network_environment(temp.path());

        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));

        assert!(matches!(app.mode, AppMode::Network(_)));
        assert!(app.network_refreshing);
        for _ in 0..100 {
            app.poll_network();
            if !app.network_refreshing {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(app.mode, AppMode::Network(_)));
        assert!(!app.network_refreshing);
    }

    #[test]
    fn network_share_prompt_owns_input_and_supports_cursor_editing() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.network_environment = test_network_environment(temp.path());
        app.mode = AppMode::Network(NetworkView {
            shares: Vec::new(),
            selected: 0,
        });

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        for character in "nas/share".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::SmbUsername { ref address, .. })
                if address.uri == "smb://nas/share"
        ));
        assert!(app.operation.is_none());
    }

    #[test]
    fn read_only_network_manager_can_open_connected_share_but_not_change_it() {
        let temp = tempfile::tempdir().unwrap();
        let mount = temp.path().join("mounted");
        std::fs::create_dir(&mount).unwrap();
        let mut app = test_app(temp.path());
        app.config.behavior.read_only = true;
        let share = NetworkShare {
            address: ShareAddress::parse("smb://nas/public").unwrap(),
            mount_path: Some(mount.clone()),
            username: None,
            domain: None,
            saved: false,
            discovered: true,
        };
        app.mode = AppMode::Network(NetworkView {
            shares: vec![share],
            selected: 0,
        });

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Network(_)));
        assert!(app.network_operation.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
        assert_eq!(app.current_dir, mount);
    }

    #[test]
    fn uppercase_m_opens_and_navigates_the_apps_launcher() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());

        app.handle_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT));
        assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 1 })));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
    }

    #[test]
    fn configured_app_hotkey_replaces_the_default() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.config = toml::from_str("[hotkeys]\ntools = 'F2'\n").unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::SHIFT));
        assert!(matches!(app.mode, AppMode::Browser));

        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));
    }

    #[test]
    fn one_device_hotkey_opens_the_unified_manager() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());

        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Partitions(_)));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Browser));
    }

    #[test]
    fn partition_view_navigation_is_modal_and_returns_to_apps() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("untouched.txt");
        std::fs::write(&file, b"safe").unwrap();
        let mut app = test_app(temp.path());
        app.partition_return_to_apps = true;
        app.mode = AppMode::Partitions(PartitionView {
            entries: Vec::new(),
            selected: 0,
            overlay: None,
        });

        for key in [KeyCode::Char('d'), KeyCode::Char('x'), KeyCode::Enter] {
            app.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
        }
        assert!(file.exists());
        assert!(matches!(app.mode, AppMode::Partitions(_)));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Apps(AppsView { selected: 0 })));
    }

    #[test]
    fn read_only_mode_opens_device_manager_for_safe_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(
            temp.path().to_path_buf(),
            ConfigLoad::Valid {
                config: Config::default(),
                path: temp.path().join("config.toml"),
            },
            true,
        );
        app.mode = AppMode::Apps(AppsView { selected: 0 });

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.mode, AppMode::Partitions(_)));
    }

    fn partition_test_view() -> PartitionView {
        let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:16\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" LABEL=\"Data\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n",
        );
        PartitionView {
            entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
            selected: 1,
            overlay: None,
        }
    }

    #[test]
    fn partition_format_flow_chooses_a_filesystem_and_optional_label() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.mode = AppMode::Partitions(partition_test_view());

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::FormatOptions { selected: 0, .. }),
                ..
            })
        ));

        for _ in 0..7 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::FormatLabel {
                    filesystem: Filesystem::Exfat,
                    ..
                }),
                ..
            })
        ));

        for character in "Archive".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Confirm {
                    action: PartitionAction::Format {
                        filesystem: Filesystem::Exfat,
                        label: Some(ref label),
                        ..
                    },
                    yes_selected: false,
                }),
                ..
            }) if label == "Archive"
        ));

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Confirm {
                    yes_selected: true,
                    ..
                }),
                ..
            })
        ));

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Actions { selected: 0 }),
                ..
            })
        ));
        assert!(app.partition_operation.is_none());
    }

    #[test]
    fn partition_format_flow_offers_luks2_and_confirms_the_passphrase_twice() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.mode = AppMode::Partitions(partition_test_view());

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::FormatLabel {
                    encrypted: true,
                    ..
                }),
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for character in "correct horse".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for character in "correct horse".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Confirm {
                    action: PartitionAction::EncryptFormat {
                        filesystem: Filesystem::Ext4,
                        ..
                    },
                    yes_selected: false,
                }),
                ..
            })
        ));
    }

    #[test]
    fn luks_passphrase_change_uses_three_masked_steps() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let fixture = "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"crypto_LUKS\" UUID=\"luks-id\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n";
        let view = PartitionView {
            entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
            selected: 0,
            overlay: None,
        };
        app.mode = app.begin_partition_task(view, PartitionTask::ChangePassphrase);
        for text in ["current key", "replacement key", "replacement key"] {
            for character in text.chars() {
                app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
            }
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        let AppMode::Partitions(PartitionView {
            overlay: Some(PartitionOverlay::Confirm { action, .. }),
            ..
        }) = &app.mode
        else {
            panic!("expected passphrase-change confirmation");
        };
        assert!(matches!(
            action,
            PartitionAction::ChangeLuksPassphrase { .. }
        ));
        let rendered = format!("{action:?}");
        assert!(!rendered.contains("current key"));
        assert!(!rendered.contains("replacement key"));
    }

    #[test]
    fn locked_luks_volume_offers_access_passphrase_and_encryption_options() {
        let temp = tempfile::tempdir().unwrap();
        let app = test_app(temp.path());
        let fixture = "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"crypto_LUKS\" UUID=\"luks-id\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n";
        let view = PartitionView {
            entries: partition::from_lsblk_fixture(fixture, &[]).unwrap().entries,
            selected: 0,
            overlay: None,
        };
        let tasks = app.partition_tasks_for_view(&view);
        assert!(tasks.contains(&PartitionTask::EncryptionAccess));
        assert!(tasks.contains(&PartitionTask::ChangePassphrase));
        assert!(tasks.contains(&PartitionTask::EncryptionOptions));
        assert!(!tasks.contains(&PartitionTask::MountOptions));
    }

    #[test]
    fn partition_tasks_are_context_sensitive_and_read_only_aware() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut view = partition_test_view();
        let partition_tasks = app.partition_tasks_for_view(&view);
        assert_eq!(
            partition_tasks,
            vec![
                PartitionTask::Mount,
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
                PartitionTask::MountOptions,
            ]
        );
        assert!(!partition_tasks.contains(&PartitionTask::CreateTable));
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::Format)
            .is_none());
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::CreatePartition)
            .unwrap()
            .contains("whole disk"));

        view.selected = 0;
        let disk_tasks = app.partition_tasks_for_view(&view);
        assert_eq!(
            disk_tasks,
            vec![
                PartitionTask::CreatePartition,
                PartitionTask::CreateTable,
                PartitionTask::CreateImage,
                PartitionTask::RestoreImage,
                PartitionTask::SmartReport,
                PartitionTask::SmartShortTest,
                PartitionTask::SmartExtendedTest,
                PartitionTask::DriveSettings,
                PartitionTask::Eject,
            ]
        );
        assert_eq!(
            app.partition_task_name(&view, PartitionTask::CreateTable),
            "Format disk"
        );
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::CreatePartition)
            .is_none());

        view.entries[0].device.table_type = None;
        let blank_disk_tasks = app.partition_tasks_for_view(&view);
        assert_eq!(
            blank_disk_tasks,
            vec![
                PartitionTask::CreateTable,
                PartitionTask::Format,
                PartitionTask::CreateImage,
                PartitionTask::RestoreImage,
                PartitionTask::SmartReport,
                PartitionTask::SmartShortTest,
                PartitionTask::SmartExtendedTest,
                PartitionTask::DriveSettings,
                PartitionTask::Eject
            ]
        );
        assert_eq!(
            app.partition_task_name(&view, PartitionTask::CreateTable),
            "Format disk"
        );
        assert_eq!(
            app.partition_task_name(&view, PartitionTask::Format),
            "Use whole disk"
        );

        app.config.behavior.read_only = true;
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::CreatePartition)
            .unwrap()
            .contains("Read-only"));
    }

    #[test]
    fn common_partition_types_map_to_exact_gpt_and_mbr_ids() {
        let temp = tempfile::tempdir().unwrap();
        let app = test_app(temp.path());
        let mut view = partition_test_view();

        assert_eq!(
            app.partition_type_id(&view, "linux").unwrap(),
            "0fc63daf-8483-4772-8e79-3d69d8477de4"
        );
        assert!(app.partition_type_id(&view, "something vague").is_err());

        view.entries[0].device.table_type = Some("dos".into());
        assert_eq!(app.partition_type_id(&view, "data").unwrap(), "07");
        assert_eq!(app.partition_type_id(&view, "af").unwrap(), "af");
    }

    #[test]
    fn read_only_mode_still_allows_checks_and_table_backups() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        app.config.behavior.read_only = true;
        let mut view = partition_test_view();

        assert!(app
            .partition_task_unavailable(&view, PartitionTask::Check)
            .is_none());
        view.selected = 0;
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::BackupTable)
            .is_none());
        assert!(app
            .partition_task_unavailable(&view, PartitionTask::CreateTable)
            .is_some());
    }

    #[test]
    fn disk_reset_chooses_empty_gpt_or_mbr_without_free_form_input() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut view = partition_test_view();
        view.selected = 0;
        view.overlay = Some(PartitionOverlay::Actions { selected: 1 });
        app.mode = AppMode::Partitions(view);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::DiskLayoutOptions { selected: 0, .. }),
                ..
            })
        ));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Confirm {
                    action: PartitionAction::EraseDisk { .. },
                    yes_selected: false,
                }),
                ..
            })
        ));

        let view = partition_test_view();
        let gpt = app.partition_disk_layout_action(
            &PartitionView {
                selected: 0,
                ..view.clone()
            },
            1,
            false,
        );
        let mbr = app.partition_disk_layout_action(
            &PartitionView {
                selected: 0,
                ..view
            },
            2,
            false,
        );
        assert!(matches!(
            gpt,
            Ok(PartitionAction::CreateTable {
                table: PartitionTable::Gpt,
                ..
            })
        ));
        assert!(matches!(
            mbr,
            Ok(PartitionAction::CreateTable {
                table: PartitionTable::Msdos,
                ..
            })
        ));
    }

    #[test]
    fn create_partition_defaults_to_max_and_accepts_a_custom_size() {
        let temp = tempfile::tempdir().unwrap();
        let app = test_app(temp.path());
        let mut view = partition_test_view();
        view.selected = 0;
        let region = partition::largest_free_region(&view.entries[0], &view.entries).unwrap();

        let (default_input, _) = app.partition_task_input(&view, PartitionTask::CreatePartition);
        assert_eq!(default_input, "max");

        let maximum = app
            .partition_action_from_input(&view, PartitionTask::CreatePartition, "max")
            .unwrap();
        assert!(matches!(
            maximum,
            PartitionAction::CreatePartition {
                start_bytes,
                end_bytes,
                ..
            } if start_bytes == region.0 && end_bytes == region.1
        ));

        let half = app
            .partition_action_from_input(&view, PartitionTask::CreatePartition, "50%")
            .unwrap();
        assert!(matches!(
            half,
            PartitionAction::CreatePartition {
                start_bytes,
                end_bytes,
                ..
            } if start_bytes == region.0
                && end_bytes > start_bytes
                && end_bytes - start_bytes <= (region.1 - region.0) / 2
                && end_bytes.is_multiple_of(512)
        ));
    }

    #[test]
    fn create_partition_flow_selects_free_space_before_size() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut view = partition_test_view();
        view.selected = 0;
        view.overlay = Some(PartitionOverlay::Actions { selected: 0 });
        app.mode = AppMode::Partitions(view);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::FreeRegionOptions { selected: 0 }),
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::PartitionSize { ref input, .. }),
                ..
            }) if input == "max"
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Partitions(PartitionView {
                overlay: Some(PartitionOverlay::Confirm {
                    action: PartitionAction::CreatePartition { .. },
                    yes_selected: false,
                }),
                ..
            })
        ));
    }

    #[test]
    fn resize_action_grows_or_shrinks_ext4_by_final_size() {
        let temp = tempfile::tempdir().unwrap();
        let app = test_app(temp.path());
        let view = partition_test_view();

        let shrink = app.partition_resize_action(&view, "50MiB").unwrap();
        assert!(matches!(shrink, PartitionAction::Shrink { .. }));
        partition::validate_snapshot(&shrink, &view.entries).unwrap();

        let grow = app.partition_resize_action(&view, "max").unwrap();
        assert!(matches!(grow, PartitionAction::Grow { .. }));
        partition::validate_snapshot(&grow, &view.entries).unwrap();
    }

    #[test]
    fn partition_confirmation_opens_masked_administrator_authentication() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let mut view = partition_test_view();
        let action = PartitionAction::Format {
            target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        if !partition::authentication_required(&action) {
            return;
        }
        view.overlay = Some(PartitionOverlay::Confirm {
            action,
            yes_selected: false,
        });
        app.mode = AppMode::Partitions(view);

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::PartitionAuthentication { .. })
        ));

        for character in "not-a-real-password".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::PartitionAuthentication { ref input, .. })
                if input.character_count() == "not-a-real-password".chars().count()
        ));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, AppMode::Partitions(_)));
        assert!(app.partition_operation.is_none());
    }

    #[test]
    fn failed_partition_authentication_returns_to_a_clean_masked_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let view = partition_test_view();
        let action = PartitionAction::Format {
            target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(PartitionUpdate::Finished(Err(
                crate::error::MinfmError::IncorrectPassphrase,
            )))
            .unwrap();
        app.partition_return_view = Some(view);
        app.partition_operation = Some(RunningPartitionOperation {
            receiver,
            started_at: Instant::now(),
            action,
        });

        assert!(app.poll_partition_operation());
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::PartitionAuthentication {
                ref input,
                error: Some(ref error),
                ..
            }) if input.is_empty() && error.contains("failed")
        ));
        assert!(app.partition_operation.is_none());
    }

    #[test]
    fn every_smart_action_opens_a_scrollable_report_and_returns_to_devices() {
        let temp = tempfile::tempdir().unwrap();
        for action_kind in 0..3 {
            let mut app = test_app(temp.path());
            let mut view = partition_test_view();
            view.selected = 0;
            let disk = DeviceIdentity::from_entry(&view.entries[0]).unwrap();
            let action = match action_kind {
                0 => PartitionAction::SmartReport { disk },
                1 => PartitionAction::SmartTest {
                    disk,
                    extended: false,
                },
                _ => PartitionAction::SmartTest {
                    disk,
                    extended: true,
                },
            };
            let (sender, receiver) = mpsc::sync_channel(1);
            let report = (1..=30)
                .map(|line| format!("SMART report line {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            sender.send(PartitionUpdate::Finished(Ok(report))).unwrap();
            app.partition_return_view = Some(view);
            app.partition_operation = Some(RunningPartitionOperation {
                receiver,
                started_at: Instant::now(),
                action,
            });

            assert!(app.poll_partition_operation());
            assert!(matches!(
                app.mode,
                AppMode::Prompt(Prompt::SmartReport {
                    ref body,
                    scroll: 0,
                    ..
                }) if body.contains("SMART report line 30")
            ));

            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            assert!(matches!(
                app.mode,
                AppMode::Prompt(Prompt::SmartReport { scroll: 1, .. })
            ));
            for _ in 0..10 {
                app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
            }
            assert!(matches!(
                app.mode,
                AppMode::Prompt(Prompt::SmartReport {
                    ref body,
                    scroll,
                    ..
                }) if scroll == smart_report_scroll_limit(body)
            ));
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(matches!(app.mode, AppMode::Partitions(_)));
        }
    }

    #[test]
    fn partition_failures_open_a_complete_error_dialog() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = test_app(temp.path());
        let view = partition_test_view();
        let action = PartitionAction::Format {
            target: DeviceIdentity::from_entry(&view.entries[1]).unwrap(),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        let message = "mkfs.ext4 failed because the device became unavailable";
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(PartitionUpdate::Finished(Err(
                crate::error::MinfmError::Message(message.into()),
            )))
            .unwrap();
        app.partition_return_view = Some(view);
        app.partition_operation = Some(RunningPartitionOperation {
            receiver,
            started_at: Instant::now(),
            action,
        });

        assert!(app.poll_partition_operation());
        assert!(matches!(
            app.mode,
            AppMode::Prompt(Prompt::PartitionError { ref body, .. })
                if body.contains("Format")
                    && body.contains("/dev/sdb1")
                    && body.contains(message)
        ));
        assert_eq!(app.status, "Partition operation failed");
        assert!(app.partition_operation.is_none());
    }
}
