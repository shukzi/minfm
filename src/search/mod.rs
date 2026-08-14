use std::{
    cmp::Ordering,
    collections::HashSet,
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{self, Read},
    os::unix::ffi::{OsStrExt, OsStringExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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

#[cfg(test)]
thread_local! {
    static CASE_FOLD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SORT_KEY_ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

use crate::{
    entry::{EntryKind, FileEntry},
    process::spawn_with_retry,
};

const UPDATE_QUEUE_CAPACITY: usize = 256;
const RG_BATCH_MAX_PATHS: usize = 128;
const RG_BATCH_MAX_ARG_BYTES: usize = 256 * 1024;
const RG_STDERR_MAX_BYTES: usize = 64 * 1024;
const RG_CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);
pub(crate) const UPDATES_PER_UI_TICK: usize = 512;

#[derive(Debug)]
struct RgBatch {
    paths: Vec<PathBuf>,
    arg_bytes: usize,
    budget: RgArgBudget,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct SearchMetrics {
    candidates_examined: usize,
    candidates_passed_metadata: usize,
    current_batch_paths: usize,
    current_batch_bytes: usize,
    max_batch_paths: usize,
    max_batch_bytes: usize,
    rg_subprocesses: usize,
    matches: usize,
    first_match_us: Option<u128>,
    rg_leader_pid: Option<u32>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct RunOptions {
    max_batch_paths_override: Option<usize>,
}

#[cfg(test)]
struct MetricsContext<'a> {
    metrics: &'a Arc<std::sync::Mutex<SearchMetrics>>,
    worker_started: std::time::Instant,
}

#[cfg(test)]
impl RunOptions {
    fn max_batch_paths(self) -> usize {
        self.max_batch_paths_override.unwrap_or(RG_BATCH_MAX_PATHS)
    }
}

impl RgBatch {
    fn new(budget: RgArgBudget) -> Self {
        Self {
            paths: Vec::new(),
            arg_bytes: budget.fixed_bytes,
            budget,
        }
    }

    fn try_push(&mut self, path: PathBuf) -> bool {
        self.try_push_with_limit(path, RG_BATCH_MAX_PATHS)
    }

    fn try_push_with_limit(&mut self, path: PathBuf, max_paths: usize) -> bool {
        if self.paths.len() == max_paths || !self.budget.can_add(self.arg_bytes, &path) {
            return false;
        }
        self.arg_bytes += encoded_arg_bytes(path.as_os_str());
        self.paths.push(path);
        true
    }

    fn clear(&mut self) {
        self.paths.clear();
        self.arg_bytes = self.budget.fixed_bytes;
    }
}

#[derive(Debug, Clone, Copy)]
struct RgArgBudget {
    fixed_bytes: usize,
}

impl RgArgBudget {
    fn new(request: &CompiledSearch, command: &RgCommand) -> Result<Self, RgError> {
        let mut fixed_bytes = encoded_arg_bytes(command.program.as_os_str());
        if request.content_mode() == ContentMode::Literal {
            fixed_bytes += encoded_arg_bytes(OsStr::new("--fixed-strings"));
        }
        for argument in ["--null", "--files-with-matches", "--no-messages"] {
            fixed_bytes += encoded_arg_bytes(OsStr::new(argument));
        }
        fixed_bytes += encoded_arg_bytes(OsStr::new(request.content()));
        fixed_bytes += encoded_arg_bytes(OsStr::new("--"));
        if fixed_bytes > RG_BATCH_MAX_ARG_BYTES {
            return Err(RgError::Failed(format!(
                "ripgrep arguments exceed the {} byte argument limit",
                RG_BATCH_MAX_ARG_BYTES
            )));
        }
        Ok(Self { fixed_bytes })
    }

    fn can_add(self, current_bytes: usize, path: &Path) -> bool {
        current_bytes
            .checked_add(encoded_arg_bytes(path.as_os_str()))
            .is_some_and(|bytes| bytes <= RG_BATCH_MAX_ARG_BYTES)
    }
}

fn encoded_arg_bytes(argument: &OsStr) -> usize {
    argument.as_bytes().len().saturating_add(1)
}

#[derive(Debug)]
enum RgError {
    Failed(String),
    Cancelled,
}

impl fmt::Display for RgError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(message) => write!(output, "{message}"),
            Self::Cancelled => write!(output, "content search cancelled"),
        }
    }
}

#[derive(Debug, Clone)]
struct RgCommand {
    program: PathBuf,
    #[cfg(test)]
    inject_supervision_error: bool,
    #[cfg(test)]
    inject_cleanup_esrch: bool,
    #[cfg(test)]
    inject_cancel_after_spawn: bool,
}

impl Default for RgCommand {
    fn default() -> Self {
        Self {
            program: PathBuf::from("rg"),
            #[cfg(test)]
            inject_supervision_error: false,
            #[cfg(test)]
            inject_cleanup_esrch: false,
            #[cfg(test)]
            inject_cancel_after_spawn: false,
        }
    }
}

pub fn ripgrep_available() -> bool {
    Command::new("rg")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_rg_batch(
    request: &CompiledSearch,
    paths: &[PathBuf],
    cancel: &AtomicBool,
) -> Result<HashSet<PathBuf>, RgError> {
    let command = rg_command(request);
    run_rg_batch_with_command(request, paths, cancel, &command)
}

#[cfg(test)]
fn rg_command(request: &CompiledSearch) -> RgCommand {
    request
        .rg_program
        .as_ref()
        .map(|program| RgCommand {
            program: program.clone(),
            inject_supervision_error: false,
            inject_cleanup_esrch: false,
            inject_cancel_after_spawn: false,
        })
        .unwrap_or_default()
}

#[cfg(not(test))]
fn rg_command(_request: &CompiledSearch) -> RgCommand {
    RgCommand::default()
}

fn run_rg_batch_with_command(
    request: &CompiledSearch,
    paths: &[PathBuf],
    cancel: &AtomicBool,
    command: &RgCommand,
) -> Result<HashSet<PathBuf>, RgError> {
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err(RgError::Cancelled);
    }
    let budget = RgArgBudget::new(request, command)?;
    let total_bytes = paths.iter().try_fold(budget.fixed_bytes, |bytes, path| {
        bytes.checked_add(encoded_arg_bytes(path.as_os_str()))
    });
    if paths.len() > RG_BATCH_MAX_PATHS
        || total_bytes.is_none_or(|bytes| bytes > RG_BATCH_MAX_ARG_BYTES)
    {
        return Err(RgError::Failed(format!(
            "ripgrep batch exceeds the {} byte argument limit",
            RG_BATCH_MAX_ARG_BYTES
        )));
    }
    let stdout_cap = paths.iter().try_fold(0_usize, |bytes, path| {
        bytes.checked_add(encoded_arg_bytes(path.as_os_str()))
    });
    let stdout_cap =
        stdout_cap.ok_or_else(|| RgError::Failed("ripgrep stdout limit overflow".into()))?;
    let pipe_flags = rustix::pipe::PipeFlags::NONBLOCK | rustix::pipe::PipeFlags::CLOEXEC;
    let (stdout_reader, stdout_writer) = rustix::pipe::pipe_with(pipe_flags).map_err(|error| {
        RgError::Failed(format!("could not create ripgrep stdout pipe: {error}"))
    })?;
    let (stderr_reader, stderr_writer) = rustix::pipe::pipe_with(pipe_flags).map_err(|error| {
        RgError::Failed(format!("could not create ripgrep stderr pipe: {error}"))
    })?;
    set_pipe_writer_blocking(&stdout_writer, "stdout")?;
    set_pipe_writer_blocking(&stderr_writer, "stderr")?;
    let mut stdout = RgOutput::new(std::fs::File::from(stdout_reader), stdout_cap, "stdout");
    let mut stderr = RgOutput::new(
        std::fs::File::from(stderr_reader),
        RG_STDERR_MAX_BYTES,
        "stderr",
    );
    let mut process = Command::new(&command.program);
    if request.content_mode() == ContentMode::Literal {
        process.arg("--fixed-strings");
    }
    process
        .args(["--null", "--files-with-matches", "--no-messages"])
        .arg(request.content())
        .arg("--")
        .args(paths.iter().map(|path| path.as_os_str()))
        .stdout(Stdio::from(stdout_writer))
        .stderr(Stdio::from(stderr_writer))
        .process_group(0);
    let mut child =
        spawn_with_retry(&mut process).map_err(|error| RgError::Failed(error.to_string()))?;
    #[cfg(test)]
    if let Some(metrics) = &request.metrics_hook {
        metrics.lock().unwrap().rg_leader_pid = Some(child.id());
    }

    #[cfg(test)]
    let mut supervision_polls = 0_usize;
    let status = loop {
        if let Err(error) = drain_rg_output_round(&mut stdout, &mut stderr) {
            drop(stdout);
            drop(stderr);
            let cleanup = cleanup_rg_process_group(&mut child, cleanup_signal_override(command));
            cleanup.map_err(|cleanup| {
                RgError::Failed(format!("{error}; ripgrep cleanup failed: {cleanup}"))
            })?;
            return Err(error);
        }
        if cancel.load(AtomicOrdering::Relaxed) || inject_cancel_after_spawn(command) {
            drop(stdout);
            drop(stderr);
            let cleanup = cleanup_rg_process_group(&mut child, cleanup_signal_override(command));
            cleanup.map_err(|error| {
                RgError::Failed(format!("ripgrep cancellation cleanup failed: {error}"))
            })?;
            return Err(RgError::Cancelled);
        }
        #[cfg(test)]
        if command.inject_supervision_error && supervision_polls == 10 {
            drop(stdout);
            drop(stderr);
            let cleanup = cleanup_rg_process_group(&mut child, cleanup_signal_override(command));
            cleanup.map_err(|error| {
                RgError::Failed(format!("ripgrep supervision cleanup failed: {error}"))
            })?;
            return Err(RgError::Failed("injected supervision failure".into()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                #[cfg(test)]
                {
                    supervision_polls += 1;
                }
                thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) => {
                drop(stdout);
                drop(stderr);
                let cleanup =
                    cleanup_rg_process_group(&mut child, cleanup_signal_override(command));
                cleanup.map_err(|cleanup| {
                    RgError::Failed(format!(
                        "ripgrep supervision failed: {error}; cleanup failed: {cleanup}"
                    ))
                })?;
                return Err(RgError::Failed(format!(
                    "ripgrep supervision failed: {error}"
                )));
            }
        }
    };
    drain_available_rg_outputs(&mut stdout, &mut stderr)?;
    let stdout = stdout.finish();
    let stderr = stderr.finish();
    match status.code() {
        Some(0) => {
            let candidates: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
            Ok(stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
                .filter(|path| candidates.contains(path.as_path()))
                .collect())
        }
        Some(1) => Ok(HashSet::new()),
        _ => Err(RgError::Failed(
            String::from_utf8_lossy(&stderr).into_owned(),
        )),
    }
}

fn set_pipe_writer_blocking(writer: &std::os::fd::OwnedFd, stream: &str) -> Result<(), RgError> {
    let mut flags = rustix::fs::fcntl_getfl(writer).map_err(|error| {
        RgError::Failed(format!("could not inspect ripgrep {stream} pipe: {error}"))
    })?;
    flags.remove(rustix::fs::OFlags::NONBLOCK);
    rustix::fs::fcntl_setfl(writer, flags).map_err(|error| {
        RgError::Failed(format!(
            "could not configure ripgrep {stream} pipe: {error}"
        ))
    })
}

struct RgOutput {
    reader: std::fs::File,
    bytes: Vec<u8>,
    limit: usize,
    stream: &'static str,
}

impl RgOutput {
    fn new(reader: std::fs::File, limit: usize, stream: &'static str) -> Self {
        Self {
            reader,
            bytes: Vec::with_capacity(limit.min(8 * 1024).saturating_add(1)),
            limit,
            stream,
        }
    }

    fn drain_once(&mut self) -> Result<bool, RgError> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let mut chunk = [0_u8; 8 * 1024];
        let read_limit = chunk.len().min(remaining.saturating_add(1));
        match self.reader.read(&mut chunk[..read_limit]) {
            Ok(0) => Ok(false),
            Ok(read) => {
                self.bytes.extend_from_slice(&chunk[..read]);
                if self.bytes.len() > self.limit {
                    return Err(RgError::Failed(format!(
                        "ripgrep {} exceeds the {} byte output limit",
                        self.stream, self.limit
                    )));
                }
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(RgError::Failed(format!(
                "could not read ripgrep {}: {error}",
                self.stream
            ))),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn drain_rg_output_round(stdout: &mut RgOutput, stderr: &mut RgOutput) -> Result<bool, RgError> {
    let stdout_progress = stdout.drain_once()?;
    let stderr_progress = stderr.drain_once()?;
    Ok(stdout_progress || stderr_progress)
}

fn drain_available_rg_outputs(stdout: &mut RgOutput, stderr: &mut RgOutput) -> Result<(), RgError> {
    loop {
        if !drain_rg_output_round(stdout, stderr)? {
            return Ok(());
        }
    }
}

#[cfg(test)]
fn cleanup_signal_override(command: &RgCommand) -> Option<rustix::io::Errno> {
    command
        .inject_cleanup_esrch
        .then_some(rustix::io::Errno::SRCH)
}

#[cfg(test)]
fn inject_cancel_after_spawn(command: &RgCommand) -> bool {
    command.inject_cancel_after_spawn
}

#[cfg(not(test))]
fn inject_cancel_after_spawn(_command: &RgCommand) -> bool {
    false
}

#[cfg(not(test))]
fn cleanup_signal_override(_command: &RgCommand) -> Option<rustix::io::Errno> {
    None
}

fn cleanup_rg_process_group(
    child: &mut std::process::Child,
    signal_override: Option<rustix::io::Errno>,
) -> Result<(), String> {
    let pid = rustix::process::Pid::from_raw(child.id() as i32)
        .ok_or_else(|| "invalid child process id".to_owned())?;
    let signal_error = signal_override
        .or_else(|| rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).err());
    if signal_error.is_some() {
        let _ = child.kill();
    }
    let deadline = std::time::Instant::now() + RG_CLEANUP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return match signal_error {
                    None | Some(rustix::io::Errno::SRCH) => Ok(()),
                    Some(error) => Err(format!("could not signal ripgrep process group: {error}")),
                };
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::park_timeout(Duration::from_millis(10));
            }
            Ok(None) => {
                return Err(signal_error.map_or_else(
                    || {
                        format!(
                            "child did not exit within {} ms",
                            RG_CLEANUP_TIMEOUT.as_millis()
                        )
                    },
                    |error| error.to_string(),
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[derive(Debug)]
pub enum SearchUpdate {
    Match(SearchHit),
    Skipped(usize),
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
    #[cfg(test)]
    return spawn_with_options(request, RunOptions::default(), None);
    #[cfg(not(test))]
    {
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
}

#[cfg(test)]
fn spawn_with_options(
    mut request: CompiledSearch,
    options: RunOptions,
    metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) -> RunningSearch {
    request.metrics_hook = metrics.clone();
    let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        run_with_options(
            request,
            match_sender,
            control_sender,
            worker_cancel,
            options,
            metrics,
        )
    });
    RunningSearch {
        receiver: SearchReceiver::new(match_receiver, control_receiver),
        cancel,
    }
}

#[cfg(test)]
pub(crate) fn running_search_for_test(updates: Vec<SearchUpdate>) -> RunningSearch {
    let (match_sender, match_receiver) = mpsc::channel();
    let (control_sender, control_receiver) = mpsc::channel();
    for update in updates {
        match update {
            SearchUpdate::Match(_) => match_sender.send(update).unwrap(),
            SearchUpdate::Skipped(_) | SearchUpdate::Finished(_) | SearchUpdate::Failed(_) => {
                control_sender.send(update).unwrap()
            }
        }
    }
    drop(match_sender);
    drop(control_sender);
    RunningSearch {
        receiver: SearchReceiver::new(match_receiver, control_receiver),
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

#[cfg(test)]
pub(crate) fn hit_for_test(path: PathBuf, query: &str) -> SearchHit {
    let metadata = fs::symlink_metadata(&path).unwrap();
    let entry = FileEntry::from_path_metadata(path, metadata).unwrap();
    let rank = smart_rank(&case_fold(query), &case_fold(&entry.name)).unwrap();
    SearchHit::new(entry, rank)
}

#[cfg(test)]
pub(crate) fn synthetic_hit_for_test(path: PathBuf, name: String) -> SearchHit {
    SearchHit::new(
        FileEntry {
            path,
            name,
            kind: EntryKind::File,
            size: 0,
            mode: 0,
            modified: None,
            selected: false,
        },
        MatchRank {
            tier: 0,
            penalty: 0,
        },
    )
}

#[cfg(test)]
pub(crate) fn reset_case_fold_calls_for_test() {
    CASE_FOLD_CALLS.set(0);
    SORT_KEY_ALLOCATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn case_fold_calls_for_test() -> usize {
    CASE_FOLD_CALLS.get()
}

#[cfg(test)]
pub(crate) fn sort_key_allocations_for_test() -> usize {
    SORT_KEY_ALLOCATIONS.get()
}

fn run(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
) {
    #[cfg(test)]
    return run_with_options(
        request,
        match_sender,
        control_sender,
        cancel,
        RunOptions::default(),
        None,
    );
    #[cfg(not(test))]
    run_inner(
        request,
        match_sender,
        control_sender,
        cancel,
        RG_BATCH_MAX_PATHS,
    )
}

#[cfg(test)]
fn run_with_options(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    options: RunOptions,
    metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) {
    run_inner(
        request,
        match_sender,
        control_sender,
        cancel,
        options.max_batch_paths(),
        metrics,
    )
}

#[cfg(not(test))]
fn run_inner(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    max_batch_paths: usize,
) {
    run_worker(
        request,
        match_sender,
        control_sender,
        cancel,
        max_batch_paths,
    )
}

#[cfg(test)]
fn run_inner(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    max_batch_paths: usize,
    metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) {
    run_worker(
        request,
        match_sender,
        control_sender,
        cancel,
        max_batch_paths,
        metrics,
    )
}

#[cfg(not(test))]
fn run_worker(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    max_batch_paths: usize,
) {
    run_worker_body(
        request,
        match_sender,
        control_sender,
        cancel,
        max_batch_paths,
    )
}

#[cfg(test)]
fn run_worker(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    max_batch_paths: usize,
    metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) {
    run_worker_body(
        request,
        match_sender,
        control_sender,
        cancel,
        max_batch_paths,
        metrics,
    )
}

fn run_worker_body(
    request: CompiledSearch,
    match_sender: SyncSender<SearchUpdate>,
    control_sender: Sender<SearchUpdate>,
    cancel: Arc<AtomicBool>,
    max_batch_paths: usize,
    #[cfg(test)] metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) {
    #[cfg(test)]
    let worker_started = std::time::Instant::now();
    #[cfg(test)]
    let metrics_context = metrics.as_ref().map(|metrics| MetricsContext {
        metrics,
        worker_started,
    });
    let mut builder = ignore::WalkBuilder::new(request.root());
    builder.follow_links(false);
    builder.require_git(false);
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
    let rg_budget = match RgArgBudget::new(&request, &rg_command(&request)) {
        Ok(budget) => budget,
        Err(error) => {
            let _ = control_sender.send(SearchUpdate::Failed(error.to_string()));
            let _ = control_sender.send(SearchUpdate::Finished(SearchCompletion {
                incomplete: true,
                ..SearchCompletion::default()
            }));
            return;
        }
    };
    let mut rg_batch = RgBatch::new(rg_budget);
    let mut rg_hits = Vec::new();
    let mut rg_error = None;
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
        #[cfg(test)]
        if let Some(metrics) = &metrics {
            metrics.lock().unwrap().candidates_examined += 1;
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
            if kind != EntryKind::File {
                continue;
            }
            #[cfg(test)]
            if let Some(metrics) = &metrics {
                metrics.lock().unwrap().candidates_passed_metadata += 1;
            }
            if !rg_batch.try_push_with_limit(entry.path.clone(), max_batch_paths) {
                if rg_batch.paths.is_empty() {
                    rg_error = Some(RgError::Failed(format!(
                        "path exceeds the {} byte ripgrep argument limit",
                        RG_BATCH_MAX_ARG_BYTES
                    )));
                    break;
                }
                match emit_rg_batch(
                    &request,
                    &mut rg_batch,
                    &mut rg_hits,
                    &match_sender,
                    Arc::as_ref(&cancel),
                    &mut matches,
                    #[cfg(test)]
                    metrics_context.as_ref(),
                ) {
                    Ok(limit_reached) => {
                        if limit_reached {
                            truncated = true;
                            break;
                        }
                    }
                    Err(error) => {
                        rg_error = Some(error);
                        break;
                    }
                }
                if !rg_batch.try_push_with_limit(entry.path.clone(), max_batch_paths) {
                    rg_error = Some(RgError::Failed(format!(
                        "path exceeds the {} byte ripgrep argument limit",
                        RG_BATCH_MAX_ARG_BYTES
                    )));
                    break;
                }
            }
            #[cfg(test)]
            update_batch_metrics(metrics.as_ref(), &rg_batch);
            rg_hits.push(SearchHit::new(entry, rank));
            continue;
        }
        #[cfg(test)]
        if let Some(counter) = &request.send_attempt_counter {
            counter.fetch_add(1, AtomicOrdering::Relaxed);
        }
        if !send_match_cancellable(
            &match_sender,
            SearchHit::new(entry, rank),
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
    if rg_error.is_none()
        && !truncated
        && !rg_batch.paths.is_empty()
        && !cancel.load(AtomicOrdering::Relaxed)
    {
        match emit_rg_batch(
            &request,
            &mut rg_batch,
            &mut rg_hits,
            &match_sender,
            Arc::as_ref(&cancel),
            &mut matches,
            #[cfg(test)]
            metrics_context.as_ref(),
        ) {
            Ok(limit_reached) => truncated = limit_reached,
            Err(error) => rg_error = Some(error),
        }
    }
    if skipped > 0 {
        let _ = control_sender.send(SearchUpdate::Skipped(skipped));
    }
    if let Some(RgError::Failed(message)) = &rg_error {
        let _ = control_sender.send(SearchUpdate::Failed(message.clone()));
    }
    let _ = control_sender.send(SearchUpdate::Finished(SearchCompletion {
        cancelled: cancel.load(AtomicOrdering::Relaxed)
            || matches!(rg_error, Some(RgError::Cancelled)),
        truncated,
        incomplete: skipped > 0 || matches!(rg_error, Some(RgError::Failed(_))),
    }));
}

fn emit_rg_batch(
    request: &CompiledSearch,
    batch: &mut RgBatch,
    hits: &mut Vec<SearchHit>,
    sender: &SyncSender<SearchUpdate>,
    cancel: &AtomicBool,
    emitted: &mut usize,
    #[cfg(test)] metrics: Option<&MetricsContext<'_>>,
) -> Result<bool, RgError> {
    #[cfg(test)]
    if let Some(metrics) = metrics {
        metrics.metrics.lock().unwrap().rg_subprocesses += 1;
    }
    let verified = run_rg_batch(request, &batch.paths, cancel)?;
    batch.clear();
    for hit in hits.drain(..) {
        if !verified.contains(&hit.entry.path) {
            continue;
        }
        #[cfg(test)]
        if let Some(counter) = &request.send_attempt_counter {
            counter.fetch_add(1, AtomicOrdering::Relaxed);
        }
        if !send_match_cancellable(sender, hit, cancel) {
            return Err(RgError::Cancelled);
        }
        *emitted += 1;
        #[cfg(test)]
        if let Some(metrics) = metrics {
            let mut values = metrics.metrics.lock().unwrap();
            values.matches += 1;
            values
                .first_match_us
                .get_or_insert_with(|| metrics.worker_started.elapsed().as_micros());
        }
        if *emitted == request.selected_result_limit() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
fn update_batch_metrics(metrics: Option<&Arc<std::sync::Mutex<SearchMetrics>>>, batch: &RgBatch) {
    if let Some(metrics) = metrics {
        let mut metrics = metrics.lock().unwrap();
        metrics.current_batch_paths = batch.paths.len();
        metrics.current_batch_bytes = batch.arg_bytes;
        metrics.max_batch_paths = metrics.max_batch_paths.max(batch.paths.len());
        metrics.max_batch_bytes = metrics.max_batch_bytes.max(batch.arg_bytes);
    }
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
    sort_name: SearchNameKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SearchNameKey {
    Utf8 { folded: String, raw: OsString },
    NonUtf8(OsString),
}

impl SearchNameKey {
    fn new(basename: OsString) -> Self {
        #[cfg(test)]
        SORT_KEY_ALLOCATIONS
            .set(SORT_KEY_ALLOCATIONS.get() + usize::from(basename.to_str().is_some()));
        match basename.to_str() {
            Some(raw) => Self::Utf8 {
                folded: case_fold(raw),
                raw: basename,
            },
            None => Self::NonUtf8(basename),
        }
    }
}

impl SearchHit {
    fn new(entry: FileEntry, rank: MatchRank) -> Self {
        let basename = entry
            .path
            .file_name()
            .unwrap_or(entry.path.as_os_str())
            .to_os_string();
        let sort_name = SearchNameKey::new(basename);
        Self {
            entry,
            rank,
            sort_name,
        }
    }

    fn refresh_sort_name(&mut self) {
        self.sort_name = Self::new(self.entry.clone(), self.rank).sort_name;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    pub retained: usize,
    pub removed: usize,
    pub renamed: bool,
}

pub fn refresh_hits(hits: &mut Vec<SearchHit>, renamed: Option<(&Path, &Path)>) -> RefreshSummary {
    let mut summary = RefreshSummary::default();
    hits.retain_mut(|hit| {
        let path = renamed
            .filter(|(old, _)| hit.entry.path == *old)
            .map_or(hit.entry.path.as_path(), |(_, new)| new);
        let Ok(metadata) = fs::symlink_metadata(path) else {
            summary.removed += 1;
            return false;
        };
        let Ok(mut entry) = FileEntry::from_path_metadata(path.to_path_buf(), metadata) else {
            summary.removed += 1;
            return false;
        };
        if entry.kind != hit.entry.kind {
            summary.removed += 1;
            return false;
        }
        entry.selected = hit.entry.selected;
        summary.renamed |= entry.path != hit.entry.path;
        hit.entry = entry;
        hit.refresh_sort_name();
        summary.retained += 1;
        true
    });
    summary
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
            .then_with(|| self.sort_name.cmp(&other.sort_name))
            .then_with(|| self.entry.path.cmp(&other.entry.path))
    }
}

#[derive(Debug, Clone)]
enum NameMatcher {
    Any,
    Smart(String),
    Glob { matcher: GlobMatcher, path: bool },
    Regex(Regex),
}

#[derive(Debug, Clone)]
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
    #[cfg(test)]
    rg_program: Option<PathBuf>,
    #[cfg(test)]
    metrics_hook: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
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
        if !self.content.is_empty() && self.content_mode == ContentMode::Regex {
            Regex::new(&self.content).map_err(|error| SearchValidationError::InvalidPattern {
                mode: "content regex",
                message: error.to_string(),
            })?;
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
            #[cfg(test)]
            rg_program: None,
            #[cfg(test)]
            metrics_hook: None,
        })
    }
}

fn parse_size(raw: &str) -> Option<Option<u64>> {
    let input = raw.trim();
    if input.is_empty() {
        return Some(None);
    }

    let (number, multiplier) = [
        ("KiB", 1_024_u64),
        ("MiB", 1_048_576),
        ("GiB", 1_073_741_824),
        ("KB", 1_000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("B", 1),
    ]
    .into_iter()
    .find_map(|(unit, multiplier)| {
        input
            .strip_suffix(unit)
            .map(|number| (number.trim_end(), multiplier))
    })
    .unwrap_or((input, 1));

    let mut components = number.split('.');
    let whole = components.next()?;
    let fraction = components.next();
    if components.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|digit| digit.is_ascii_digit())
        || fraction.is_some_and(|digits| {
            digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit())
        })
    {
        return None;
    }

    let whole_bytes = whole.parse::<u64>().ok()?.checked_mul(multiplier)?;
    let fractional_bytes = if let Some(digits) = fraction {
        let fraction = digits.parse::<u64>().ok()?;
        let scale = (0..digits.len()).try_fold(1_u64, |scale, _| scale.checked_mul(10))?;
        let common_factor = gcd(scale, multiplier);
        let reduced_scale = scale / common_factor;
        if fraction % reduced_scale != 0 {
            return None;
        }
        (fraction / reduced_scale).checked_mul(multiplier / common_factor)?
    } else {
        0
    };
    whole_bytes.checked_add(fractional_bytes).map(Some)
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
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
        if (self.size.minimum.is_some() || self.size.maximum.is_some())
            && kind == EntryKind::Directory
        {
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
    #[cfg(test)]
    CASE_FOLD_CALLS.set(CASE_FOLD_CALLS.get() + 1);
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
mod tests;
