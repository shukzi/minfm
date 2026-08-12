use std::{
    cmp::Ordering,
    error::Error,
    ffi::OsStr,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, SystemTime},
};

use chrono::{Local, NaiveDate, TimeZone};
use globset::{Glob, GlobMatcher};
use regex::Regex;
use unicode_casefold::UnicodeCaseFold;

use crate::entry::{EntryKind, FileEntry};

const UPDATE_QUEUE_CAPACITY: usize = 256;
pub(crate) const UPDATES_PER_UI_TICK: usize = 512;

#[derive(Debug)]
pub enum SearchUpdate {
    Match(SearchHit),
    Skipped,
    Finished(SearchCompletion),
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchCompletion {
    pub cancelled: bool,
    pub truncated: bool,
    pub incomplete: bool,
}

pub struct RunningSearch {
    pub receiver: SearchReceiver,
    pub cancel: Arc<AtomicBool>,
}

pub struct SearchReceiver {
    matches: Receiver<SearchUpdate>,
    control: Receiver<SearchUpdate>,
}

impl SearchReceiver {
    fn new(matches: Receiver<SearchUpdate>, control: Receiver<SearchUpdate>) -> Self {
        Self { matches, control }
    }

    pub fn try_recv(&self) -> Result<SearchUpdate, TryRecvError> {
        match self.matches.try_recv() {
            Ok(update) => return Ok(update),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                return self.control.try_recv();
            }
        }
        match self.control.try_recv() {
            Ok(update) => Ok(update),
            Err(TryRecvError::Empty) => Err(TryRecvError::Empty),
            Err(TryRecvError::Disconnected) => match self.matches.try_recv() {
                Ok(update) => Ok(update),
                Err(TryRecvError::Empty) => Err(TryRecvError::Empty),
                Err(TryRecvError::Disconnected) => Err(TryRecvError::Disconnected),
            },
        }
    }

    pub fn recv(&self) -> Result<SearchUpdate, mpsc::RecvError> {
        loop {
            match self.try_recv() {
                Ok(update) => return Ok(update),
                Err(TryRecvError::Disconnected) => return Err(mpsc::RecvError),
                Err(TryRecvError::Empty) => thread::park_timeout(Duration::from_millis(1)),
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<SearchUpdate, mpsc::RecvTimeoutError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_recv() {
                Ok(update) => return Ok(update),
                Err(TryRecvError::Disconnected) => {
                    return Err(mpsc::RecvTimeoutError::Disconnected);
                }
                Err(TryRecvError::Empty) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(mpsc::RecvTimeoutError::Timeout);
                    }
                    thread::park_timeout((deadline - now).min(Duration::from_millis(1)));
                }
            }
        }
    }
}

pub fn spawn(request: CompiledSearch) -> RunningSearch {
    let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || run(request, match_sender, control_sender, worker_cancel));
    RunningSearch {
        receiver: SearchReceiver::new(match_receiver, control_receiver),
        cancel,
    }
}

fn run(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
) {
    let mut builder = ignore::WalkBuilder::new(request.root());
    builder.follow_links(false);
    if request.scope() == SearchScope::CurrentDirectory {
        builder.max_depth(Some(1));
    }
    if request.include_ignored_hidden() {
        builder
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false);
    }
    if request.scope() == SearchScope::Filesystem {
        builder.filter_entry(|entry| filesystem_entry_allowed(entry.path()));
    }

    let mut skipped = 0_usize;
    let mut matches = 0_usize;
    let mut truncated = false;
    for item in builder.build() {
        if cancel.load(AtomicOrdering::Relaxed) {
            break;
        }
        let item = match item {
            Ok(item) => item,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let path = item.path();
        if path == request.root() {
            continue;
        }
        if request.scope() == SearchScope::CurrentDirectory
            && item
                .file_type()
                .is_some_and(|file_type| file_type.is_symlink())
        {
            continue;
        }
        let relative = path.strip_prefix(request.root()).unwrap_or(path);
        let basename = path.file_name().unwrap_or(path.as_os_str());
        let Some(rank) = request.matches_name(relative, basename) else {
            continue;
        };
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let kind = FileEntry::kind_from_metadata(&metadata);
        let modified = metadata.modified().ok();
        if !request.matches_metadata(kind, metadata.len(), modified) {
            continue;
        }
        #[cfg(test)]
        if let Some(counter) = &request.construction_counter {
            counter.fetch_add(1, AtomicOrdering::Relaxed);
        }
        let entry = match FileEntry::from_path_metadata(path.to_path_buf(), metadata) {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if !request.content().is_empty() {
            continue;
        }
        #[cfg(test)]
        if let Some(counter) = &request.send_attempt_counter {
            counter.fetch_add(1, AtomicOrdering::Relaxed);
        }
        if !send_match_cancellable(
            &match_sender,
            SearchHit { entry, rank },
            Arc::as_ref(&cancel),
        ) {
            return;
        }
        matches += 1;
        if matches == request.selected_result_limit() {
            truncated = true;
            break;
        }
    }
    if skipped > 0 {
        let _ = control_sender.send(SearchUpdate::Skipped);
    }
    let _ = control_sender.send(SearchUpdate::Finished(SearchCompletion {
        cancelled: cancel.load(AtomicOrdering::Relaxed),
        truncated,
        incomplete: skipped > 0,
    }));
}

fn send_match_cancellable(
    sender: &SyncSender<SearchUpdate>,
    hit: SearchHit,
    cancel: &AtomicBool,
) -> bool {
    let mut update = SearchUpdate::Match(hit);
    loop {
        if cancel.load(AtomicOrdering::Relaxed) {
            return true;
        }
        match sender.try_send(update) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                update = returned;
                thread::yield_now();
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn filesystem_entry_allowed(path: &Path) -> bool {
    if path == Path::new("/proc")
        || path.starts_with("/proc/")
        || path == Path::new("/sys")
        || path.starts_with("/sys/")
        || path == Path::new("/dev")
        || path.starts_with("/dev/")
    {
        return false;
    }
    path == Path::new("/run")
        || path == Path::new("/run/media")
        || path.starts_with("/run/media/")
        || !path.starts_with("/run/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    CurrentDirectory,
    RecursiveHere,
    Filesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMode {
    Smart,
    Glob,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentMode {
    Literal,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryKinds(u8);

impl EntryKinds {
    pub const ANY: Self = Self(0);
    pub const FILES: Self = Self(1 << 0);
    pub const DIRECTORIES: Self = Self(1 << 1);
    pub const SYMLINKS: Self = Self(1 << 2);
    pub const BLOCK_DEVICES: Self = Self(1 << 3);
    pub const OTHER: Self = Self(1 << 4);

    const fn bit(kind: EntryKind) -> u8 {
        match kind {
            EntryKind::File => Self::FILES.0,
            EntryKind::Directory => Self::DIRECTORIES.0,
            EntryKind::Symlink => Self::SYMLINKS.0,
            EntryKind::BlockDevice => Self::BLOCK_DEVICES.0,
            EntryKind::Other => Self::OTHER.0,
        }
    }

    pub fn toggle(&mut self, kind: EntryKind) {
        self.0 ^= Self::bit(kind);
    }

    pub fn contains(self, kind: EntryKind) -> bool {
        self == Self::ANY || self.0 & Self::bit(kind) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeBounds {
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeBounds {
    pub after: Option<SystemTime>,
    pub before: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // Names are the specified user-facing limits.
pub enum ResultLimit {
    OneThousand,
    FiveThousand,
    TenThousand,
}

impl ResultLimit {
    pub fn get(self) -> usize {
        match self {
            Self::OneThousand => 1_000,
            Self::FiveThousand => 5_000,
            Self::TenThousand => 10_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchDraft {
    pub root: PathBuf,
    pub scope: SearchScope,
    pub name: String,
    pub name_mode: NameMode,
    pub content: String,
    pub content_mode: ContentMode,
    pub types: EntryKinds,
    pub minimum_size: String,
    pub maximum_size: String,
    pub modified_after: String,
    pub modified_before: String,
    pub include_ignored_hidden: bool,
    pub result_limit: ResultLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchValidationError {
    Unconstrained,
    InvalidSize { field: &'static str, value: String },
    SizeOrder,
    InvalidTime { field: &'static str, value: String },
    TimeOrder,
    InvalidPattern { mode: &'static str, message: String },
    RipgrepRequired,
}

impl fmt::Display for SearchValidationError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unconstrained => write!(output, "enter a search or choose a filter"),
            Self::InvalidSize { field, value } => write!(output, "invalid {field}: {value}"),
            Self::SizeOrder => write!(output, "minimum size must not exceed maximum size"),
            Self::InvalidTime { field, value } => write!(output, "invalid {field}: {value}"),
            Self::TimeOrder => write!(output, "modified-after must not exceed modified-before"),
            Self::InvalidPattern { mode, message } => write!(output, "invalid {mode}: {message}"),
            Self::RipgrepRequired => write!(output, "content search requires ripgrep"),
        }
    }
}

impl Error for SearchValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchRank {
    tier: u8,
    penalty: u32,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub entry: FileEntry,
    pub rank: MatchRank,
}

impl PartialEq for SearchHit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SearchHit {}

impl PartialOrd for SearchHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchHit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank
            .cmp(&other.rank)
            .then_with(|| compare_basenames(&self.entry.path, &other.entry.path))
            .then_with(|| self.entry.path.cmp(&other.entry.path))
    }
}

fn compare_basenames(left: &Path, right: &Path) -> Ordering {
    let left = left.file_name().unwrap_or(left.as_os_str());
    let right = right.file_name().unwrap_or(right.as_os_str());
    match (left.to_str(), right.to_str()) {
        (Some(left), Some(right)) => case_fold(left).cmp(&case_fold(right)),
        _ => left.cmp(right),
    }
}

#[derive(Debug)]
enum NameMatcher {
    Any,
    Smart(String),
    Glob { matcher: GlobMatcher, path: bool },
    Regex(Regex),
}

#[derive(Debug)]
pub struct CompiledSearch {
    root: PathBuf,
    scope: SearchScope,
    matcher: NameMatcher,
    content: String,
    content_mode: ContentMode,
    types: EntryKinds,
    size: SizeBounds,
    time: TimeBounds,
    include_ignored_hidden: bool,
    result_limit: ResultLimit,
    #[cfg(test)]
    result_limit_override: Option<usize>,
    #[cfg(test)]
    construction_counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
    #[cfg(test)]
    send_attempt_counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl SearchDraft {
    pub fn quick(root: PathBuf) -> Self {
        Self::advanced(root, SearchScope::CurrentDirectory)
    }

    pub fn advanced(root: PathBuf, scope: SearchScope) -> Self {
        Self {
            root,
            scope,
            name: String::new(),
            name_mode: NameMode::Smart,
            content: String::new(),
            content_mode: ContentMode::Literal,
            types: EntryKinds::ANY,
            minimum_size: String::new(),
            maximum_size: String::new(),
            modified_after: String::new(),
            modified_before: String::new(),
            include_ignored_hidden: false,
            result_limit: ResultLimit::FiveThousand,
        }
    }

    pub fn compile(&self, rg_available: bool) -> Result<CompiledSearch, SearchValidationError> {
        if self.content.is_empty()
            && self.name.is_empty()
            && self.types == EntryKinds::ANY
            && self.minimum_size.trim().is_empty()
            && self.maximum_size.trim().is_empty()
            && self.modified_after.trim().is_empty()
            && self.modified_before.trim().is_empty()
        {
            return Err(SearchValidationError::Unconstrained);
        }
        if !self.content.is_empty() && !rg_available {
            return Err(SearchValidationError::RipgrepRequired);
        }

        let minimum =
            parse_size(&self.minimum_size).ok_or_else(|| SearchValidationError::InvalidSize {
                field: "minimum size",
                value: self.minimum_size.clone(),
            })?;
        let maximum =
            parse_size(&self.maximum_size).ok_or_else(|| SearchValidationError::InvalidSize {
                field: "maximum size",
                value: self.maximum_size.clone(),
            })?;
        if minimum.zip(maximum).is_some_and(|(min, max)| min > max) {
            return Err(SearchValidationError::SizeOrder);
        }

        let now = SystemTime::now();
        let after = parse_time(&self.modified_after, now, false).ok_or_else(|| {
            SearchValidationError::InvalidTime {
                field: "modified after",
                value: self.modified_after.clone(),
            }
        })?;
        let before = parse_time(&self.modified_before, now, true).ok_or_else(|| {
            SearchValidationError::InvalidTime {
                field: "modified before",
                value: self.modified_before.clone(),
            }
        })?;
        if after.zip(before).is_some_and(|(start, end)| start > end) {
            return Err(SearchValidationError::TimeOrder);
        }

        let matcher = compile_name(self.name_mode, &self.name)?;
        Ok(CompiledSearch {
            root: self.root.clone(),
            scope: self.scope,
            matcher,
            content: self.content.clone(),
            content_mode: self.content_mode,
            types: self.types,
            size: SizeBounds { minimum, maximum },
            time: TimeBounds { after, before },
            include_ignored_hidden: self.include_ignored_hidden,
            result_limit: self.result_limit,
            #[cfg(test)]
            result_limit_override: None,
            #[cfg(test)]
            construction_counter: None,
            #[cfg(test)]
            send_attempt_counter: None,
        })
    }
}

fn parse_size(raw: &str) -> Option<Option<u64>> {
    let input = raw.trim();
    if input.is_empty() {
        return Some(None);
    }
    let (number, multiplier) = [
        ("KiB", 1024_u64),
        ("MiB", 1024_u64.pow(2)),
        ("GiB", 1024_u64.pow(3)),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        input
            .strip_suffix(suffix)
            .map(|number| (number.trim(), multiplier))
    })
    .unwrap_or((input, 1));
    if number.is_empty() || number.starts_with('-') {
        return None;
    }
    number
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
        .map(Some)
}

fn parse_time(raw: &str, now: SystemTime, end_of_day: bool) -> Option<Option<SystemTime>> {
    let input = raw.trim();
    if input.is_empty() {
        return Some(None);
    }
    if let Some(days) = input.strip_suffix('d') {
        if days.is_empty() || days.starts_with('-') {
            return None;
        }
        return now
            .checked_sub(Duration::from_secs(
                days.parse::<u64>().ok()?.checked_mul(86_400)?,
            ))
            .map(Some);
    }
    let date = NaiveDate::parse_from_str(input, "%Y-%m-%d").ok()?;
    if end_of_day {
        let next_day = date.succ_opt()?.and_hms_opt(0, 0, 0)?;
        let next_start: SystemTime = Local.from_local_datetime(&next_day).single()?.into();
        next_start.checked_sub(Duration::from_nanos(1)).map(Some)
    } else {
        let naive = date.and_hms_opt(0, 0, 0)?;
        let start: SystemTime = Local.from_local_datetime(&naive).single()?.into();
        Some(Some(start))
    }
}

fn compile_name(mode: NameMode, pattern: &str) -> Result<NameMatcher, SearchValidationError> {
    if pattern.is_empty() {
        return Ok(NameMatcher::Any);
    }
    match mode {
        NameMode::Smart => Ok(NameMatcher::Smart(case_fold(pattern))),
        NameMode::Glob => Glob::new(pattern)
            .map(|glob| NameMatcher::Glob {
                matcher: glob.compile_matcher(),
                path: pattern.contains('/'),
            })
            .map_err(|error| SearchValidationError::InvalidPattern {
                mode: "glob",
                message: error.to_string(),
            }),
        NameMode::Regex => Regex::new(pattern)
            .map(NameMatcher::Regex)
            .map_err(|error| SearchValidationError::InvalidPattern {
                mode: "regex",
                message: error.to_string(),
            }),
    }
}

impl CompiledSearch {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn scope(&self) -> SearchScope {
        self.scope
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn content_mode(&self) -> ContentMode {
        self.content_mode
    }
    pub fn include_ignored_hidden(&self) -> bool {
        self.include_ignored_hidden
    }
    pub fn result_limit(&self) -> ResultLimit {
        self.result_limit
    }

    fn selected_result_limit(&self) -> usize {
        #[cfg(test)]
        if let Some(limit) = self.result_limit_override {
            return limit;
        }
        self.result_limit.get()
    }

    pub fn matches_name(&self, relative_path: &Path, basename: &OsStr) -> Option<MatchRank> {
        match &self.matcher {
            NameMatcher::Any => Some(MatchRank {
                tier: 0,
                penalty: 0,
            }),
            NameMatcher::Glob { matcher, path } => matcher
                .is_match(if *path {
                    relative_path
                } else {
                    Path::new(basename)
                })
                .then_some(MatchRank {
                    tier: 0,
                    penalty: 0,
                }),
            NameMatcher::Regex(regex) => basename.to_str().and_then(|basename| {
                regex.is_match(basename).then_some(MatchRank {
                    tier: 0,
                    penalty: 0,
                })
            }),
            NameMatcher::Smart(query) => basename
                .to_str()
                .and_then(|basename| smart_rank(query, &case_fold(basename))),
        }
    }

    pub fn matches_metadata(
        &self,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
    ) -> bool {
        if !self.types.contains(kind) {
            return false;
        }
        if (self.size.minimum.is_some() || self.size.maximum.is_some()) && kind != EntryKind::File {
            return false;
        }
        if self.size.minimum.is_some_and(|minimum| size < minimum)
            || self.size.maximum.is_some_and(|maximum| size > maximum)
        {
            return false;
        }
        if self.time.after.is_some() || self.time.before.is_some() {
            let Some(modified) = modified else {
                return false;
            };
            if self.time.after.is_some_and(|after| modified < after)
                || self.time.before.is_some_and(|before| modified > before)
            {
                return false;
            }
        }
        true
    }
}

fn case_fold(text: &str) -> String {
    text.case_fold().collect()
}

fn smart_rank(query: &str, candidate: &str) -> Option<MatchRank> {
    if candidate == query {
        return Some(MatchRank {
            tier: 0,
            penalty: 0,
        });
    }
    if candidate.starts_with(query) {
        return Some(MatchRank {
            tier: 1,
            penalty: (candidate.len() - query.len()) as u32,
        });
    }
    if let Some(position) = candidate.find(query) {
        return Some(MatchRank {
            tier: 2,
            penalty: position as u32,
        });
    }
    fuzzy_penalty(query, candidate).map(|penalty| MatchRank { tier: 3, penalty })
}

fn fuzzy_penalty(query: &str, candidate: &str) -> Option<u32> {
    let query: Vec<char> = query.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let length_difference = query.len().abs_diff(candidate.len()) as u32;
    let threshold = (query.len() as u32 / 3).max(1) + length_difference;
    if query.len() == candidate.len() {
        let mismatches: Vec<usize> = query
            .iter()
            .zip(&candidate)
            .enumerate()
            .filter_map(|(index, (left, right))| (left != right).then_some(index))
            .collect();
        if let [left, right] = mismatches.as_slice() {
            if *right == *left + 1
                && query[*left] == candidate[*right]
                && query[*right] == candidate[*left]
            {
                return Some(1);
            }
        }
    }
    let mut next = 0;
    let mut gaps = 0_u32;
    for wanted in &query {
        let found = candidate[next..].iter().position(|value| value == wanted)?;
        gaps += found as u32;
        next += found + 1;
    }
    let penalty = gaps + (candidate.len() - next) as u32 + length_difference;
    (penalty <= threshold).then_some(penalty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use std::{
        collections::BTreeSet,
        ffi::{OsStr, OsString},
        fs,
        os::unix::ffi::OsStringExt,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
    };

    struct TraversalFixture {
        temp: tempfile::TempDir,
    }

    impl TraversalFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            fs::create_dir(temp.path().join("nested")).unwrap();
            fs::write(temp.path().join("top.txt"), "top").unwrap();
            fs::write(temp.path().join("nested/deep.txt"), "deep").unwrap();
            fs::write(temp.path().join(".hidden.txt"), "hidden").unwrap();
            fs::write(temp.path().join("ignored.txt"), "ignored").unwrap();
            fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
            symlink(temp.path(), temp.path().join("loop")).unwrap();
            Self { temp }
        }

        fn root(&self) -> &Path {
            self.temp.path()
        }
    }

    fn traversal_request(
        root: &Path,
        scope: SearchScope,
        include_ignored_hidden: bool,
    ) -> CompiledSearch {
        let mut draft = SearchDraft::advanced(root.to_path_buf(), scope);
        draft.name_mode = NameMode::Regex;
        draft.name = r"^(top\.txt|deep\.txt|\.hidden\.txt|ignored\.txt|loop)$".into();
        draft.include_ignored_hidden = include_ignored_hidden;
        draft.compile(true).unwrap()
    }

    fn run_traversal(request: CompiledSearch) -> (Vec<SearchHit>, SearchCompletion) {
        let running = spawn(request);
        let mut hits = Vec::new();
        loop {
            match running.receiver.recv().unwrap() {
                SearchUpdate::Match(hit) => hits.push(hit),
                SearchUpdate::Skipped => {}
                SearchUpdate::Finished(completion) => return (hits, completion),
                SearchUpdate::Failed(message) => panic!("search failed: {message}"),
            }
        }
    }

    fn paths(root: &Path, hits: &[SearchHit]) -> BTreeSet<PathBuf> {
        hits.iter()
            .map(|hit| hit.entry.path.strip_prefix(root).unwrap().to_path_buf())
            .collect()
    }

    fn count_named(hits: &[SearchHit], name: &str) -> usize {
        hits.iter().filter(|hit| hit.entry.name == name).count()
    }

    fn compiled_name(mode: NameMode, name: &str) -> CompiledSearch {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name_mode = mode;
        draft.name = name.into();
        draft.compile(true).unwrap()
    }

    fn compiled_filters(minimum: &str, maximum: &str, after: &str, before: &str) -> CompiledSearch {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "report".into();
        draft.minimum_size = minimum.into();
        draft.maximum_size = maximum.into();
        draft.modified_after = after.into();
        draft.modified_before = before.into();
        draft.compile(true).unwrap()
    }

    fn local_time(value: &str) -> SystemTime {
        let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).single().unwrap().into()
    }

    #[test]
    fn empty_search_requires_content_or_a_non_default_filter() {
        let root = PathBuf::from("/tmp/root");
        assert_eq!(
            SearchDraft::advanced(root.clone(), SearchScope::RecursiveHere)
                .compile(true)
                .unwrap_err(),
            SearchValidationError::Unconstrained
        );

        let mut filtered = SearchDraft::advanced(root, SearchScope::RecursiveHere);
        filtered.types = EntryKinds::FILES;
        assert!(filtered.compile(true).is_ok());
    }

    #[test]
    fn size_and_time_bounds_must_be_ordered() {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "report".into();
        draft.minimum_size = "20 MiB".into();
        draft.maximum_size = "10 MiB".into();
        assert_eq!(
            draft.compile(true).unwrap_err(),
            SearchValidationError::SizeOrder
        );
    }

    #[test]
    fn content_search_requires_ripgrep() {
        let mut draft = SearchDraft::advanced(PathBuf::from("/tmp"), SearchScope::RecursiveHere);
        draft.content = "needle".into();
        assert_eq!(
            draft.compile(false).unwrap_err(),
            SearchValidationError::RipgrepRequired
        );
    }

    #[test]
    fn smart_matching_ranks_exact_prefix_substring_then_fuzzy() {
        let search = compiled_name(NameMode::Smart, "report");
        let exact = search
            .matches_name(Path::new("report"), OsStr::new("report"))
            .unwrap();
        let prefix = search
            .matches_name(Path::new("report-old"), OsStr::new("report-old"))
            .unwrap();
        let contains = search
            .matches_name(Path::new("annual-report"), OsStr::new("annual-report"))
            .unwrap();
        let fuzzy = search
            .matches_name(Path::new("rpeort"), OsStr::new("rpeort"))
            .unwrap();
        assert!(exact < prefix && prefix < contains && contains < fuzzy);
    }

    #[test]
    fn slash_globs_match_relative_paths_but_plain_globs_match_basenames() {
        assert!(compiled_name(NameMode::Glob, "src/*.rs")
            .matches_name(Path::new("src/main.rs"), OsStr::new("main.rs"))
            .is_some());
        assert!(compiled_name(NameMode::Glob, "*.rs")
            .matches_name(Path::new("src/main.rs"), OsStr::new("main.rs"))
            .is_some());
    }

    #[test]
    fn size_filters_exclude_directories_and_time_bounds_are_inclusive() {
        let search = compiled_filters("10", "20", "2026-08-12", "2026-08-12");
        let day_start = local_time("2026-08-12 00:00:00");
        let day_end = local_time("2026-08-12 23:59:59");
        assert!(!search.matches_metadata(EntryKind::Directory, 15, Some(day_start)));
        assert!(search.matches_metadata(EntryKind::File, 10, Some(day_start)));
        assert!(search.matches_metadata(EntryKind::File, 20, Some(day_end)));
    }

    #[test]
    fn parsers_accept_supported_forms_and_reject_invalid_values() {
        for value in ["1", "1 KiB", "2 MiB", "3 GiB"] {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name = "x".into();
            draft.minimum_size = value.into();
            assert!(draft.compile(true).is_ok(), "{value}");
        }
        for value in ["-1", "1 KB", "1 TiB", "word"] {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name = "x".into();
            draft.minimum_size = value.into();
            assert!(
                matches!(
                    draft.compile(true),
                    Err(SearchValidationError::InvalidSize { .. })
                ),
                "{value}"
            );
        }
        for value in ["2026-08-12", "7d", "30d"] {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name = "x".into();
            draft.modified_after = value.into();
            assert!(draft.compile(true).is_ok(), "{value}");
        }
        for value in ["-7d", "7 days", "2026-02-30"] {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name = "x".into();
            draft.modified_after = value.into();
            assert!(
                matches!(
                    draft.compile(true),
                    Err(SearchValidationError::InvalidTime { .. })
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn invalid_glob_and_regex_are_validation_errors() {
        for (mode, pattern) in [(NameMode::Glob, "["), (NameMode::Regex, "(")] {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name_mode = mode;
            draft.name = pattern.into();
            assert!(matches!(
                draft.compile(true),
                Err(SearchValidationError::InvalidPattern { .. })
            ));
        }
    }

    #[test]
    fn smart_matching_handles_empty_unicode_long_and_case_only_inputs() {
        let cases = [
            ("", "anything", true),
            ("RÉSUMÉ", "résumé", true),
            ("report", "REPORT", true),
            ("abcdefghijabcdefghij", "abcdefghijabcdefghij-extra", true),
            ("report", "unrelated", false),
        ];
        for (query, candidate, expected) in cases {
            let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
            draft.name = query.into();
            if query.is_empty() {
                draft.types = EntryKinds::FILES;
            }
            let search = draft.compile(true).unwrap();
            assert_eq!(
                search
                    .matches_name(Path::new(candidate), OsStr::new(candidate))
                    .is_some(),
                expected
            );
        }
    }

    #[test]
    fn smart_matching_uses_full_unicode_case_folding() {
        let search = compiled_name(NameMode::Smart, "Straße");
        assert!(search
            .matches_name(Path::new("STRASSE"), OsStr::new("STRASSE"))
            .is_some());
    }

    #[test]
    fn invalid_utf8_basenames_do_not_collide_through_replacement_characters() {
        let invalid_a = OsString::from_vec(vec![b'a', 0x80]);
        let invalid_b = OsString::from_vec(vec![b'a', 0x81]);
        let replacement = compiled_name(NameMode::Smart, "a�");
        assert!(replacement
            .matches_name(Path::new(&invalid_a), &invalid_a)
            .is_none());
        assert!(replacement
            .matches_name(Path::new(&invalid_b), &invalid_b)
            .is_none());

        let glob = compiled_name(NameMode::Glob, "a�");
        assert!(glob
            .matches_name(Path::new(&invalid_a), &invalid_a)
            .is_none());
        assert!(glob
            .matches_name(Path::new(&invalid_b), &invalid_b)
            .is_none());

        let regex = compiled_name(NameMode::Regex, "^a�$");
        assert!(regex
            .matches_name(Path::new(&invalid_a), &invalid_a)
            .is_none());
        assert!(regex
            .matches_name(Path::new(&invalid_b), &invalid_b)
            .is_none());
    }

    #[test]
    fn hit_ties_use_preserved_path_basenames_before_full_paths() {
        let rank = MatchRank {
            tier: 0,
            penalty: 0,
        };
        let first_name = OsString::from_vec(vec![b'a', 0x80]);
        let second_name = OsString::from_vec(vec![b'a', 0x81]);
        let mut first = PathBuf::from("/z");
        first.push(&first_name);
        let mut second = PathBuf::from("/a");
        second.push(&second_name);
        let make_hit = |path: PathBuf, display_name: &str| SearchHit {
            entry: FileEntry {
                path,
                name: display_name.into(),
                kind: EntryKind::File,
                size: 0,
                mode: 0,
                modified: None,
                selected: false,
            },
            rank,
        };
        let first = make_hit(first, "same replacement display");
        let second = make_hit(second, "same replacement display");
        assert!(first < second);
    }

    #[test]
    fn traversal_current_directory_does_not_descend() {
        let fixture = TraversalFixture::new();
        let (hits, completion) = run_traversal(traversal_request(
            fixture.root(),
            SearchScope::CurrentDirectory,
            false,
        ));

        assert_eq!(
            paths(fixture.root(), &hits),
            BTreeSet::from(["top.txt".into()])
        );
        assert_eq!(completion, SearchCompletion::default());
    }

    #[test]
    fn traversal_recursive_honors_ignores_and_does_not_follow_symlinks() {
        let fixture = TraversalFixture::new();
        let (hits, completion) = run_traversal(traversal_request(
            fixture.root(),
            SearchScope::RecursiveHere,
            false,
        ));

        assert_eq!(
            paths(fixture.root(), &hits),
            BTreeSet::from(["top.txt".into(), "nested/deep.txt".into(), "loop".into()])
        );
        assert_eq!(count_named(&hits, "deep.txt"), 1);
        assert_eq!(completion, SearchCompletion::default());
    }

    #[test]
    fn traversal_include_ignored_hidden_disables_all_ignore_filters() {
        let fixture = TraversalFixture::new();
        let (hits, completion) = run_traversal(traversal_request(
            fixture.root(),
            SearchScope::RecursiveHere,
            true,
        ));
        let paths = paths(fixture.root(), &hits);

        assert!(paths.contains(Path::new(".hidden.txt")));
        assert!(paths.contains(Path::new("ignored.txt")));
        assert_eq!(count_named(&hits, "deep.txt"), 1);
        assert_eq!(completion, SearchCompletion::default());
    }

    #[test]
    fn traversal_filesystem_prunes_virtual_trees_but_keeps_run_media() {
        assert!(!filesystem_entry_allowed(Path::new("/proc")));
        assert!(!filesystem_entry_allowed(Path::new("/proc/123/status")));
        assert!(!filesystem_entry_allowed(Path::new("/sys/class")));
        assert!(!filesystem_entry_allowed(Path::new("/dev/pts")));
        assert!(filesystem_entry_allowed(Path::new("/run")));
        assert!(!filesystem_entry_allowed(Path::new("/run/lock")));
        assert!(filesystem_entry_allowed(Path::new("/run/media")));
        assert!(filesystem_entry_allowed(Path::new("/run/media/user/disk")));
        assert!(filesystem_entry_allowed(Path::new("/home/user")));
    }

    #[test]
    fn bounded_results_stop_at_the_selected_limit_and_report_truncation() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..8 {
            fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
        }
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "item".into();
        let mut request = draft.compile(true).unwrap();
        request.result_limit_override = Some(3);

        let (hits, completion) = run_traversal(request);

        assert_eq!(hits.len(), 3);
        assert_eq!(
            completion,
            SearchCompletion {
                truncated: true,
                ..SearchCompletion::default()
            }
        );
    }

    #[test]
    fn cancellation_pre_set_before_traversal_emits_no_matches() {
        let fixture = TraversalFixture::new();
        let request = traversal_request(fixture.root(), SearchScope::RecursiveHere, true);
        let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
        let (control_sender, control_receiver) = mpsc::channel();
        let receiver = SearchReceiver::new(match_receiver, control_receiver);
        let cancel = Arc::new(AtomicBool::new(true));

        run(request, match_sender, control_sender, cancel);
        let mut updates = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(update) => updates.push(update),
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => break,
            }
        }

        assert!(updates
            .iter()
            .all(|update| !matches!(update, SearchUpdate::Match(_))));
        assert!(matches!(
            updates.last(),
            Some(SearchUpdate::Finished(SearchCompletion {
                cancelled: true,
                ..
            }))
        ));
    }

    #[test]
    fn cancellation_after_first_update_completes_promptly() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..1_000 {
            fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
        }
        let mut request =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
        request.name = "item".into();
        let running = spawn(request.compile(true).unwrap());
        let started = std::time::Instant::now();
        assert!(matches!(
            running.receiver.recv().unwrap(),
            SearchUpdate::Match(_)
        ));
        running.cancel.store(true, AtomicOrdering::Relaxed);

        let completion = loop {
            match running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            {
                SearchUpdate::Finished(completion) => break completion,
                SearchUpdate::Failed(message) => panic!("search failed: {message}"),
                SearchUpdate::Match(_) | SearchUpdate::Skipped => {}
            }
        };
        assert!(completion.cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancellation_exits_when_the_update_queue_is_saturated() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..(UPDATE_QUEUE_CAPACITY + 32) {
            fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
        }
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "item".into();
        let mut request = draft.compile(true).unwrap();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        request.send_attempt_counter = Some(Arc::clone(&attempts));
        let (sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
        let (control_sender, control_receiver) = mpsc::channel();
        let receiver = SearchReceiver::new(match_receiver, control_receiver);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (done_sender, done_receiver) = mpsc::channel();
        thread::spawn(move || {
            run(request, sender, control_sender, worker_cancel);
            done_sender.send(()).unwrap();
        });

        let saturation_deadline = std::time::Instant::now() + Duration::from_secs(2);
        while attempts.load(AtomicOrdering::Relaxed) <= UPDATE_QUEUE_CAPACITY {
            assert!(
                std::time::Instant::now() < saturation_deadline,
                "worker did not saturate the update queue"
            );
            thread::yield_now();
        }
        cancel.store(true, AtomicOrdering::Relaxed);

        done_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut matches = 0;
        let completion = loop {
            match receiver.try_recv() {
                Ok(SearchUpdate::Match(_)) => matches += 1,
                Ok(SearchUpdate::Skipped) => {}
                Ok(SearchUpdate::Finished(completion)) => break completion,
                Ok(SearchUpdate::Failed(message)) => panic!("search failed: {message}"),
                Err(mpsc::TryRecvError::Empty) => thread::yield_now(),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("receiver disconnected before completion")
                }
            }
        };
        assert_eq!(matches, UPDATE_QUEUE_CAPACITY);
        assert!(completion.cancelled);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn saturated_receiver_drains_matches_then_aggregate_skip_and_finished() {
        let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
        let (control_sender, control_receiver) = mpsc::channel();
        let receiver = SearchReceiver::new(match_receiver, control_receiver);
        for index in 0..UPDATE_QUEUE_CAPACITY {
            let path = PathBuf::from(format!("item-{index}.txt"));
            match_sender
                .send(SearchUpdate::Match(SearchHit {
                    entry: FileEntry {
                        name: path.to_string_lossy().into_owned(),
                        path,
                        kind: EntryKind::File,
                        size: 0,
                        mode: 0,
                        modified: None,
                        selected: false,
                    },
                    rank: MatchRank {
                        tier: 0,
                        penalty: 0,
                    },
                }))
                .unwrap();
        }
        control_sender.send(SearchUpdate::Skipped).unwrap();
        control_sender
            .send(SearchUpdate::Finished(SearchCompletion {
                truncated: true,
                incomplete: true,
                ..SearchCompletion::default()
            }))
            .unwrap();
        drop(match_sender);
        drop(control_sender);

        for _ in 0..UPDATE_QUEUE_CAPACITY {
            assert!(matches!(receiver.try_recv(), Ok(SearchUpdate::Match(_))));
        }
        assert!(matches!(receiver.try_recv(), Ok(SearchUpdate::Skipped)));
        assert!(matches!(
            receiver.try_recv(),
            Ok(SearchUpdate::Finished(SearchCompletion {
                truncated: true,
                incomplete: true,
                ..
            }))
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn saturated_normal_completion_preserves_truncation() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..UPDATE_QUEUE_CAPACITY {
            fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
        }
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "item".into();
        let mut request = draft.compile(true).unwrap();
        request.result_limit_override = Some(UPDATE_QUEUE_CAPACITY);
        let running = spawn(request);

        let mut matches = 0;
        let completion = loop {
            match running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            {
                SearchUpdate::Match(_) => matches += 1,
                SearchUpdate::Skipped => {}
                SearchUpdate::Finished(completion) => break completion,
                SearchUpdate::Failed(message) => panic!("search failed: {message}"),
            }
        };

        assert_eq!(matches, UPDATE_QUEUE_CAPACITY);
        assert!(completion.truncated);
        assert!(!completion.cancelled);
    }

    #[test]
    fn metadata_rejections_do_not_construct_file_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("too-small.txt"), "x").unwrap();
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
        draft.name = "too-small".into();
        draft.minimum_size = "2".into();
        let mut request = draft.compile(true).unwrap();
        let constructions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        request.construction_counter = Some(Arc::clone(&constructions));

        let (hits, completion) = run_traversal(request);

        assert!(hits.is_empty());
        assert_eq!(completion, SearchCompletion::default());
        assert_eq!(constructions.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn missing_root_is_reported_as_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let mut draft = SearchDraft::advanced(missing, SearchScope::RecursiveHere);
        draft.name = "anything".into();

        let (hits, completion) = run_traversal(draft.compile(true).unwrap());

        assert!(hits.is_empty());
        assert!(completion.incomplete);
    }

    #[test]
    fn content_requests_do_not_emit_unverified_filename_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("report.txt"), "not the requested content").unwrap();
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
        draft.name = "report".into();
        draft.content = "needle".into();

        let (hits, completion) = run_traversal(draft.compile(true).unwrap());

        assert!(hits.is_empty());
        assert_eq!(completion, SearchCompletion::default());
    }

    #[test]
    fn type_mask_toggles_and_filters_each_entry_kind() {
        let kinds = [
            EntryKind::File,
            EntryKind::Directory,
            EntryKind::Symlink,
            EntryKind::BlockDevice,
            EntryKind::Other,
        ];
        for selected in kinds {
            let mut mask = EntryKinds::ANY;
            mask.toggle(selected);
            for candidate in kinds {
                assert_eq!(mask.contains(candidate), selected == candidate);
            }
            mask.toggle(selected);
            assert_eq!(mask, EntryKinds::ANY);
        }
    }

    #[test]
    fn hits_sort_by_rank_case_folded_name_then_absolute_path() {
        fn hit(name: &str, path: &str, rank: MatchRank) -> SearchHit {
            SearchHit {
                entry: FileEntry {
                    path: PathBuf::from(path),
                    name: name.into(),
                    kind: EntryKind::File,
                    size: 0,
                    mode: 0,
                    modified: None,
                    selected: false,
                },
                rank,
            }
        }
        let best = MatchRank {
            tier: 0,
            penalty: 0,
        };
        let worse = MatchRank {
            tier: 1,
            penalty: 0,
        };
        let mut hits = [
            hit("beta", "/root/beta", best),
            hit("Alpha", "/z/Alpha", best),
            hit("alpha", "/a/alpha", best),
            hit("aardvark", "/root/aardvark", worse),
        ];
        hits.sort();
        let paths: Vec<_> = hits.iter().map(|hit| hit.entry.path.as_path()).collect();
        assert_eq!(
            paths,
            [
                Path::new("/a/alpha"),
                Path::new("/z/Alpha"),
                Path::new("/root/beta"),
                Path::new("/root/aardvark")
            ]
        );
    }
}
