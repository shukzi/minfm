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

use crate::entry::{EntryKind, FileEntry};

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
    let mut child = process
        .spawn()
        .map_err(|error| RgError::Failed(error.to_string()))?;

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
    request: CompiledSearch,
    options: RunOptions,
    metrics: Option<Arc<std::sync::Mutex<SearchMetrics>>>,
) -> RunningSearch {
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
    SearchHit { entry, rank }
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
            rg_hits.push(SearchHit { entry, rank });
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
            #[cfg(test)]
            rg_program: None,
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

    const BENCHMARK_RUNS: usize = 9;
    const BENCHMARK_BATCH_SIZES: [usize; 4] = [1, 32, 64, 128];

    #[derive(Clone, Copy)]
    struct BenchmarkFixtureSpec {
        total_files: usize,
        rg_candidates: usize,
        matching_files: usize,
        retention_results: usize,
    }

    fn benchmark_fixture_spec() -> BenchmarkFixtureSpec {
        BenchmarkFixtureSpec {
            total_files: 256,
            rg_candidates: 128,
            matching_files: 32,
            retention_results: 20_000,
        }
    }

    #[test]
    fn benchmark_matrix_covers_required_bounded_comparisons() {
        assert_eq!(BENCHMARK_BATCH_SIZES, [1, 32, 64, 128]);
        assert_eq!(BENCHMARK_RUNS, 9);
        assert!(benchmark_fixture_spec().total_files > benchmark_fixture_spec().rg_candidates);
        assert!(benchmark_fixture_spec().matching_files > 0);
        assert!(benchmark_fixture_spec().retention_results > ResultLimit::TenThousand.get());
    }

    #[test]
    fn search_metrics_record_streaming_worker_boundaries() {
        let metrics = SearchMetrics::default();
        assert_eq!(metrics.candidates_examined, 0);
        assert_eq!(RunOptions::default().max_batch_paths(), RG_BATCH_MAX_PATHS);
    }

    #[test]
    fn refresh_hits_updates_metadata_rename_marks_and_removes_stale_kinds() {
        let temp = tempfile::tempdir().unwrap();
        let retained = temp.path().join("retained.txt");
        let deleted = temp.path().join("deleted.txt");
        let old = temp.path().join("old.txt");
        let renamed = temp.path().join("renamed.txt");
        let replaced = temp.path().join("replaced.txt");
        for path in [&retained, &deleted, &old, &replaced] {
            fs::write(path, b"x").unwrap();
        }
        let mut hits = [&retained, &deleted, &old, &replaced]
            .into_iter()
            .map(|path| hit_for_test(path.clone(), "txt"))
            .collect::<Vec<_>>();
        hits[0].entry.selected = true;
        hits[2].entry.selected = true;
        fs::write(&retained, b"longer contents").unwrap();
        fs::remove_file(&deleted).unwrap();
        fs::rename(&old, &renamed).unwrap();
        fs::remove_file(&replaced).unwrap();
        fs::create_dir(&replaced).unwrap();

        let summary = refresh_hits(&mut hits, Some((&old, &renamed)));

        assert_eq!(
            summary,
            RefreshSummary {
                retained: 2,
                removed: 2,
                renamed: true
            }
        );
        assert_eq!(hits[0].entry.size, b"longer contents".len() as u64);
        assert!(hits[0].entry.selected);
        assert_eq!(hits[1].entry.path, renamed);
        assert!(hits[1].entry.selected);
    }
    use chrono::NaiveDateTime;
    use std::{
        collections::BTreeSet,
        ffi::{OsStr, OsString},
        fs,
        os::unix::ffi::OsStringExt,
        os::unix::fs::{symlink, PermissionsExt},
        path::{Path, PathBuf},
    };

    struct FakeRg {
        _temp: tempfile::TempDir,
        command: RgCommand,
        capture: PathBuf,
    }

    impl FakeRg {
        fn capturing_arguments() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let program = temp.path().join("rg");
            let capture = temp.path().join("arguments");
            fs::write(
                &program,
                format!(
                    "#!/bin/sh\n: > '{}'\nafter_separator=\nfor argument do\n  printf '%s\\0' \"$argument\" >> '{}'\n  if [ \"$after_separator\" = yes ]; then\n    printf '%s\\0' \"$argument\"\n  fi\n  if [ \"$argument\" = -- ]; then after_separator=yes; fi\ndone\n",
                    capture.display(),
                    capture.display()
                ),
            )
            .unwrap();
            let mut permissions = fs::metadata(&program).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&program, permissions).unwrap();
            Self {
                _temp: temp,
                command: RgCommand {
                    program,
                    inject_supervision_error: false,
                    inject_cleanup_esrch: false,
                    inject_cancel_after_spawn: false,
                },
                capture,
            }
        }

        fn from_script(script: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let program = temp.path().join("rg");
            let capture = temp.path().join("capture");
            fs::write(
                &program,
                script.replace("$CAPTURE", &capture.to_string_lossy()),
            )
            .unwrap();
            let mut permissions = fs::metadata(&program).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&program, permissions).unwrap();
            Self {
                _temp: temp,
                command: RgCommand {
                    program,
                    inject_supervision_error: false,
                    inject_cleanup_esrch: false,
                    inject_cancel_after_spawn: false,
                },
                capture,
            }
        }

        fn arguments(&self) -> Vec<OsString> {
            fs::read(&self.capture)
                .unwrap()
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .map(|argument| OsString::from_vec(argument.to_vec()))
                .collect()
        }
    }

    fn wait_for_pids(capture: &Path, count: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "rg did not record PIDs"
            );
            if let Ok(contents) = fs::read_to_string(capture) {
                let pids: Vec<_> = contents.lines().map(str::to_owned).collect();
                if pids.len() == count {
                    return pids;
                }
            }
            thread::yield_now();
        }
    }

    fn assert_processes_gone(pids: &[String]) {
        #[cfg(target_os = "linux")]
        for pid in pids {
            assert!(!Path::new("/proc").join(pid).exists(), "PID {pid} remains");
        }
    }

    fn wait_for_processes_gone(pids: &[String]) {
        #[cfg(target_os = "linux")]
        {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while pids.iter().any(|pid| Path::new("/proc").join(pid).exists()) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "escaped writer did not exit after reader closure"
                );
                thread::yield_now();
            }
        }
    }

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
                SearchUpdate::Skipped(_) => {}
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
    fn rg_arguments_literal_mode_are_safe_and_explicit() {
        let fake = FakeRg::capturing_arguments();
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "literal needle".into();
        let request = draft.compile(true).unwrap();
        let candidates = [PathBuf::from("/tmp/one file"), PathBuf::from("-leading")];

        let matches = run_rg_batch_with_command(
            &request,
            &candidates,
            &AtomicBool::new(false),
            &fake.command,
        )
        .unwrap();

        assert_eq!(matches, HashSet::from(candidates.clone()));
        assert_eq!(
            fake.arguments(),
            [
                "--fixed-strings".into(),
                "--null".into(),
                "--files-with-matches".into(),
                "--no-messages".into(),
                "literal needle".into(),
                "--".into(),
                candidates[0].as_os_str().to_owned(),
                candidates[1].as_os_str().to_owned(),
            ]
        );
    }

    #[test]
    fn rg_arguments_regex_mode_omits_fixed_strings() {
        let fake = FakeRg::capturing_arguments();
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle.+thread".into();
        draft.content_mode = ContentMode::Regex;
        let request = draft.compile(true).unwrap();
        let candidates = [PathBuf::from("/tmp/one")];

        run_rg_batch_with_command(
            &request,
            &candidates,
            &AtomicBool::new(false),
            &fake.command,
        )
        .unwrap();

        assert_eq!(
            fake.arguments(),
            [
                "--null".into(),
                "--files-with-matches".into(),
                "--no-messages".into(),
                "needle.+thread".into(),
                "--".into(),
                candidates[0].as_os_str().to_owned(),
            ]
        );
    }

    #[test]
    fn rg_unusual_paths_round_trip_and_binary_files_are_excluded() {
        let temp = tempfile::tempdir().unwrap();
        let names = [
            OsString::from("with space"),
            OsString::from("with\ttab"),
            OsString::from("with\nnewline"),
            OsString::from("-leading"),
            OsString::from_vec(vec![b'i', b'n', b'v', 0x80]),
        ];
        let mut paths = Vec::new();
        for name in names {
            let path = temp.path().join(name);
            fs::write(&path, "find this needle").unwrap();
            paths.push(path);
        }
        let binary = temp.path().join("binary");
        fs::write(&binary, b"\0binary contents without the query").unwrap();
        paths.push(binary.clone());
        let mut draft = SearchDraft::quick(temp.path().to_path_buf());
        draft.content = "needle".into();

        let matches = run_rg_batch(
            &draft.compile(true).unwrap(),
            &paths,
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(matches.len(), 5);
        assert!(paths[..5].iter().all(|path| matches.contains(path)));
        assert!(!matches.contains(&binary));
    }

    #[test]
    fn rg_cancellation_kills_and_reaps_the_child() {
        let fake =
            FakeRg::from_script("#!/bin/sh\necho $$ > '$CAPTURE'\nwhile :; do sleep 1; done\n");
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let command = fake.command.clone();
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || {
            run_rg_batch_with_command(
                &request,
                &[PathBuf::from("/tmp/candidate")],
                &worker_cancel,
                &command,
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let pid = loop {
            assert!(std::time::Instant::now() < deadline, "rg did not launch");
            if let Ok(pid) = fs::read_to_string(&fake.capture) {
                if !pid.trim().is_empty() {
                    break pid;
                }
            }
            thread::yield_now();
        };
        let pid = pid.trim();
        cancel.store(true, AtomicOrdering::Relaxed);

        assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(2));
        #[cfg(target_os = "linux")]
        assert!(!Path::new("/proc").join(pid).exists());
    }

    #[test]
    fn rg_cancellation_kills_descendants_that_inherit_pipes() {
        let fake = FakeRg::from_script(
            "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let command = fake.command.clone();
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || {
            run_rg_batch_with_command(
                &request,
                &[PathBuf::from("/tmp/candidate")],
                &worker_cancel,
                &command,
            )
        });
        let pids = wait_for_pids(&fake.capture, 2);
        cancel.store(true, AtomicOrdering::Relaxed);

        assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_processes_gone(&pids);
    }

    #[test]
    fn rg_supervision_error_cleans_up_the_process_group() {
        let fake = FakeRg::from_script(
            "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let mut command = fake.command.clone();
        command.inject_supervision_error = true;
        let started = std::time::Instant::now();
        let worker = thread::spawn(move || {
            run_rg_batch_with_command(
                &request,
                &[PathBuf::from("/tmp/candidate")],
                &AtomicBool::new(false),
                &command,
            )
        });
        let pids = wait_for_pids(&fake.capture, 2);
        let result = worker.join().unwrap();

        assert!(matches!(result, Err(RgError::Failed(message)) if message.contains("supervision")));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_processes_gone(&pids);
    }

    #[test]
    fn rg_normal_completion_does_not_wait_for_escaped_output_holder() {
        if Command::new("setsid").arg("--version").output().is_err() {
            return;
        }
        let fake = FakeRg::from_script(
            "#!/bin/sh\nsetsid sh -c 'sleep 1' &\necho $! > '$CAPTURE'\nexit 1\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let started = std::time::Instant::now();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );

        assert!(matches!(result, Ok(matches) if matches.is_empty()));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn rg_cancellation_does_not_wait_for_escaped_output_holder() {
        if Command::new("setsid").arg("--version").output().is_err() {
            return;
        }
        let fake = FakeRg::from_script(
            "#!/bin/sh\nsetsid sh -c 'sleep 0.1; while printf x; do :; done' &\necho $! > '$CAPTURE'\nwait\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let command = fake.command.clone();
        let worker = thread::spawn(move || {
            run_rg_batch_with_command(
                &request,
                &[PathBuf::from("/tmp/candidate")],
                &worker_cancel,
                &command,
            )
        });
        let pids = wait_for_pids(&fake.capture, 1);
        let started = std::time::Instant::now();
        cancel.store(true, AtomicOrdering::Relaxed);

        assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
        assert!(started.elapsed() < Duration::from_millis(500));
        wait_for_processes_gone(&pids);
    }

    #[test]
    fn rg_cancellation_race_treats_esrch_after_reap_as_cancelled() {
        let fake = FakeRg::from_script("#!/bin/sh\nexit 0\n");
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let mut command = fake.command.clone();
        command.inject_cleanup_esrch = true;
        command.inject_cancel_after_spawn = true;

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &command,
        );

        assert!(matches!(result, Err(RgError::Cancelled)));
    }

    #[test]
    fn rg_rejects_stdout_larger_than_the_current_batch_can_encode() {
        let fake = FakeRg::from_script("#!/bin/sh\nhead -c 70000 /dev/zero\nexit 0\n");
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );

        assert!(
            matches!(result, Err(RgError::Failed(message)) if message.contains("stdout") && message.contains("limit"))
        );
    }

    #[test]
    fn rg_rejects_stderr_larger_than_the_diagnostic_cap() {
        let fake = FakeRg::from_script("#!/bin/sh\nhead -c 70000 /dev/zero >&2\nexit 2\n");
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );

        assert!(
            matches!(result, Err(RgError::Failed(message)) if message.contains("stderr") && message.contains("limit"))
        );
    }

    #[test]
    fn rg_escaped_writer_exits_after_parent_closes_stream_readers() {
        if Command::new("setsid").arg("--version").output().is_err() {
            return;
        }
        let fake = FakeRg::from_script(
            "#!/bin/sh\nsetsid sh -c 'trap \"\" PIPE; while printf x; do :; done; exit 0' &\necho $! > '$CAPTURE'\nexit 1\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let started = std::time::Instant::now();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );
        let pids = wait_for_pids(&fake.capture, 1);

        assert!(
            matches!(result, Err(RgError::Failed(message)) if message.contains("stdout") && message.contains("limit"))
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        wait_for_processes_gone(&pids);
    }

    #[test]
    fn rg_simultaneous_stream_flood_is_bounded_without_starvation() {
        let fake = FakeRg::from_script(
            "#!/bin/sh\n(head -c 70000 /dev/zero >&2) &\nhead -c 70000 /dev/zero\nwait\n",
        );
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let started = std::time::Instant::now();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );

        assert!(
            matches!(result, Err(RgError::Failed(message)) if message.contains("output limit"))
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn rg_argument_budget_accepts_exact_limit_and_rejects_one_byte_more() {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let command = RgCommand::default();
        let budget = RgArgBudget::new(&request, &command).unwrap();
        let exact = PathBuf::from(OsString::from_vec(vec![
            b'x';
            RG_BATCH_MAX_ARG_BYTES
                - budget.fixed_bytes
                - 1
        ]));
        let too_large = PathBuf::from(OsString::from_vec(vec![
            b'x';
            RG_BATCH_MAX_ARG_BYTES
                - budget.fixed_bytes
        ]));

        assert!(budget.can_add(budget.fixed_bytes, &exact));
        assert!(!budget.can_add(budget.fixed_bytes, &too_large));
    }

    #[test]
    fn rg_argument_budget_rejects_oversized_pattern_before_spawn() {
        let fake = FakeRg::capturing_arguments();
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "x".repeat(RG_BATCH_MAX_ARG_BYTES);
        let request = draft.compile(true).unwrap();

        let result = run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &fake.command,
        );

        assert!(
            matches!(result, Err(RgError::Failed(message)) if message.contains("argument limit"))
        );
        assert!(!fake.capture.exists());
    }

    #[test]
    fn rg_batch_applies_count_ceiling_even_when_byte_budget_remains() {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.content = "needle".into();
        let request = draft.compile(true).unwrap();
        let budget = RgArgBudget::new(&request, &RgCommand::default()).unwrap();
        let mut batch = RgBatch::new(budget);

        for index in 0..RG_BATCH_MAX_PATHS {
            assert!(batch.try_push(PathBuf::from(format!("p{index}"))));
        }
        assert!(!batch.try_push(PathBuf::from("one-too-many")));
        assert!(batch.arg_bytes < RG_BATCH_MAX_ARG_BYTES);
    }

    #[test]
    fn content_search_emits_only_rg_verified_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("matching.txt"), "contains needle").unwrap();
        fs::write(temp.path().join("other.txt"), "nothing relevant").unwrap();
        fs::create_dir(temp.path().join("directory.txt")).unwrap();
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
        draft.name = ".txt".into();
        draft.content = "needle".into();

        let (hits, completion) = run_traversal(draft.compile(true).unwrap());

        assert_eq!(
            paths(temp.path(), &hits),
            BTreeSet::from(["matching.txt".into()])
        );
        assert_eq!(completion, SearchCompletion::default());
    }

    #[test]
    fn content_failure_preserves_completed_batches_and_reports_incomplete() {
        let fake = FakeRg::from_script(
            "#!/bin/sh\ncount_file='$CAPTURE'\ncount=0\n[ ! -f \"$count_file\" ] || count=$(cat \"$count_file\")\ncount=$((count + 1))\necho \"$count\" > \"$count_file\"\nif [ \"$count\" -eq 1 ]; then\n  after=\n  for argument do\n    if [ \"$after\" = yes ]; then printf '%s\\0' \"$argument\"; fi\n    [ \"$argument\" = -- ] && after=yes\n  done\n  exit 0\nfi\necho 'bad pattern' >&2\nexit 2\n",
        );
        let temp = tempfile::tempdir().unwrap();
        for index in 0..(RG_BATCH_MAX_PATHS + 1) {
            fs::write(temp.path().join(format!("item-{index}.txt")), "needle").unwrap();
        }
        let mut draft =
            SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
        draft.name = "item".into();
        draft.content = "needle".into();
        let mut request = draft.compile(true).unwrap();
        request.rg_program = Some(fake.command.program.clone());
        let running = spawn(request);
        let mut matches = 0;
        let mut failure = None;
        let mut completion = None;
        while let Ok(update) = running.receiver.recv() {
            match update {
                SearchUpdate::Match(_) => matches += 1,
                SearchUpdate::Skipped(_) => {}
                SearchUpdate::Failed(message) => failure = Some(message),
                SearchUpdate::Finished(value) => completion = Some(value),
            }
        }

        assert_eq!(matches, RG_BATCH_MAX_PATHS);
        assert!(failure
            .as_deref()
            .is_some_and(|message| message.contains("bad pattern")));
        assert_eq!(
            completion,
            Some(SearchCompletion {
                incomplete: true,
                ..SearchCompletion::default()
            })
        );
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
                SearchUpdate::Match(_) | SearchUpdate::Skipped(_) => {}
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
                Ok(SearchUpdate::Skipped(_)) => {}
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
        control_sender.send(SearchUpdate::Skipped(7)).unwrap();
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
        assert!(matches!(receiver.try_recv(), Ok(SearchUpdate::Skipped(7))));
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
                SearchUpdate::Skipped(_) => {}
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
    fn cloned_compiled_search_preserves_request_and_matching() {
        let root = PathBuf::from("/tmp/search-clone");
        let mut draft = SearchDraft::advanced(root.clone(), SearchScope::RecursiveHere);
        draft.name = "Report".into();
        draft.types = EntryKinds::FILES;
        draft.minimum_size = "10".into();
        draft.result_limit = ResultLimit::TenThousand;
        let request = draft.compile(true).unwrap();
        let cloned = request.clone();

        assert_eq!(cloned.root(), root);
        assert_eq!(cloned.scope(), SearchScope::RecursiveHere);
        assert_eq!(cloned.result_limit(), ResultLimit::TenThousand);
        assert_eq!(
            cloned.matches_name(Path::new("Report.txt"), OsStr::new("Report.txt")),
            request.matches_name(Path::new("Report.txt"), OsStr::new("Report.txt"))
        );
        assert_eq!(
            cloned.matches_metadata(EntryKind::File, 10, None),
            request.matches_metadata(EntryKind::File, 10, None)
        );
        assert_eq!(
            cloned.matches_metadata(EntryKind::Directory, 10, None),
            request.matches_metadata(EntryKind::Directory, 10, None)
        );
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

    #[derive(Clone, Copy, Debug)]
    struct BenchmarkSample {
        wall_us: u128,
        cpu_us: u128,
        first_us: u128,
    }

    fn process_cpu_us() -> u128 {
        let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return 0;
        };
        let values = fields.split_whitespace().collect::<Vec<_>>();
        // /proc/self/stat fields 14-17 are this process's utime/stime plus
        // cutime/cstime accumulated from waited-for children, including rg.
        let ticks = [11, 12, 13, 14]
            .into_iter()
            .filter_map(|index| values.get(index)?.parse::<u128>().ok())
            .sum::<u128>();
        static TICKS_PER_SECOND: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
        let hz = *TICKS_PER_SECOND.get_or_init(|| {
            Command::new("getconf")
                .arg("CLK_TCK")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(100)
        });
        ticks.saturating_mul(1_000_000) / hz
    }

    fn median(mut values: Vec<u128>) -> u128 {
        values.sort_unstable();
        values[values.len() / 2]
    }

    fn median_sample(samples: &[BenchmarkSample]) -> BenchmarkSample {
        BenchmarkSample {
            wall_us: median(samples.iter().map(|sample| sample.wall_us).collect()),
            cpu_us: median(samples.iter().map(|sample| sample.cpu_us).collect()),
            first_us: median(samples.iter().map(|sample| sample.first_us).collect()),
        }
    }

    fn create_benchmark_fixture() -> tempfile::TempDir {
        let spec = benchmark_fixture_spec();
        let temp = tempfile::tempdir().unwrap();
        for index in 0..spec.total_files {
            let directory = temp.path().join(format!("group-{:02}", index % 16));
            fs::create_dir_all(&directory).unwrap();
            let candidate = index < spec.rg_candidates;
            let name = if candidate {
                format!("report-{index:04}.txt")
            } else {
                format!("other-{index:04}.txt")
            };
            let contents = if index < spec.matching_files {
                "deterministic benchmark needle\n"
            } else {
                "deterministic benchmark haystack\n"
            };
            fs::write(directory.join(name), contents).unwrap();
        }
        temp
    }

    fn filename_sample(root: &Path) -> (BenchmarkSample, usize) {
        let mut draft = SearchDraft::advanced(root.to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "report".into();
        let cpu_started = process_cpu_us();
        let started = std::time::Instant::now();
        let running = spawn(draft.compile(true).unwrap());
        let mut first = None;
        let mut count = 0;
        loop {
            match running.receiver.recv().unwrap() {
                SearchUpdate::Match(_) => {
                    first.get_or_insert_with(|| started.elapsed().as_micros());
                    count += 1;
                }
                SearchUpdate::Skipped(_) => {}
                SearchUpdate::Finished(completion) => {
                    assert_eq!(completion, SearchCompletion::default());
                    break;
                }
                SearchUpdate::Failed(message) => panic!("search failed: {message}"),
            }
        }
        (
            BenchmarkSample {
                wall_us: started.elapsed().as_micros(),
                cpu_us: process_cpu_us().saturating_sub(cpu_started),
                first_us: first.expect("fixture must produce a filename result"),
            },
            count,
        )
    }

    #[derive(Clone, Copy, Debug)]
    struct StreamingSample {
        timing: BenchmarkSample,
        candidates_examined: usize,
        candidates_passed: usize,
        max_batch_paths: usize,
        max_batch_bytes: usize,
        subprocesses: usize,
        matches: usize,
        retained_results: usize,
    }

    fn streaming_content_sample(request: CompiledSearch, batch_size: usize) -> StreamingSample {
        let metrics = Arc::new(std::sync::Mutex::new(SearchMetrics::default()));
        let cpu_started = process_cpu_us();
        let started = std::time::Instant::now();
        let running = spawn_with_options(
            request,
            RunOptions {
                max_batch_paths_override: Some(batch_size),
            },
            Some(Arc::clone(&metrics)),
        );
        let mut retained_results = 0;
        loop {
            match running.receiver.recv().unwrap() {
                SearchUpdate::Match(_) => retained_results += 1,
                SearchUpdate::Skipped(_) => {}
                SearchUpdate::Finished(completion) => {
                    assert_eq!(completion, SearchCompletion::default());
                    break;
                }
                SearchUpdate::Failed(message) => panic!("search failed: {message}"),
            }
        }
        let metrics = metrics.lock().unwrap();
        StreamingSample {
            timing: BenchmarkSample {
                wall_us: started.elapsed().as_micros(),
                cpu_us: process_cpu_us().saturating_sub(cpu_started),
                first_us: metrics.first_match_us.expect("fixture must match"),
            },
            candidates_examined: metrics.candidates_examined,
            candidates_passed: metrics.candidates_passed_metadata,
            max_batch_paths: metrics.max_batch_paths,
            max_batch_bytes: metrics.max_batch_bytes,
            subprocesses: metrics.rg_subprocesses,
            matches: metrics.matches,
            retained_results,
        }
    }

    fn cancellation_sample(
        request: &CompiledSearch,
        candidates: &[PathBuf],
        batch_size: usize,
    ) -> u128 {
        let fake = FakeRg::from_script(
            "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let request = request.clone();
        let command = fake.command.clone();
        let batch = candidates[..batch_size].to_vec();
        let worker = thread::spawn(move || {
            run_rg_batch_with_command(&request, &batch, &worker_cancel, &command)
        });
        let pids = wait_for_pids(&fake.capture, 2);
        let started = std::time::Instant::now();
        cancel.store(true, AtomicOrdering::Relaxed);
        assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(2));
        assert_processes_gone(&pids);
        elapsed.as_micros()
    }

    #[cfg(target_os = "linux")]
    fn direct_child_pids() -> Vec<String> {
        fs::read_to_string("/proc/self/task/1/children")
            .or_else(|_| {
                fs::read_to_string(format!("/proc/self/task/{}/children", std::process::id()))
            })
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    fn real_rg_cancellation_sample(request: CompiledSearch) -> u128 {
        let metrics = Arc::new(std::sync::Mutex::new(SearchMetrics::default()));
        let running =
            spawn_with_options(request, RunOptions::default(), Some(Arc::clone(&metrics)));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while metrics.lock().unwrap().rg_subprocesses == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "real rg did not launch"
            );
            thread::yield_now();
        }
        thread::park_timeout(Duration::from_millis(2));
        #[cfg(target_os = "linux")]
        let observed_children = direct_child_pids();
        let started = std::time::Instant::now();
        running.cancel.store(true, AtomicOrdering::Relaxed);
        let completion = loop {
            match running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            {
                SearchUpdate::Match(_) | SearchUpdate::Skipped(_) => {}
                SearchUpdate::Failed(message) => panic!("real rg cancellation failed: {message}"),
                SearchUpdate::Finished(completion) => break completion,
            }
        };
        assert!(completion.cancelled);
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(2));
        #[cfg(target_os = "linux")]
        for pid in observed_children {
            assert!(
                !Path::new("/proc").join(pid).exists(),
                "real rg child remains"
            );
        }
        elapsed.as_micros()
    }

    /// Manual baseline (2026-08-12, release build, nine warm-cache runs):
    /// first result 1,074 us; completion 1,082 us on the 256-file fixture.
    /// Compare trends, not absolute timings, across machines and filesystems.
    #[test]
    #[ignore]
    fn benchmark_search_filename() {
        let fixture = create_benchmark_fixture();
        let mut samples = Vec::with_capacity(BENCHMARK_RUNS);
        for _ in 0..BENCHMARK_RUNS {
            let (sample, matches) = filename_sample(fixture.path());
            assert_eq!(matches, benchmark_fixture_spec().rg_candidates);
            samples.push(sample);
        }
        let sample = median_sample(&samples);
        eprintln!(
            "PERF search_filename_first_result_us={} search_filename_complete_us={} search_filename_cpu_us={}",
            sample.first_us, sample.wall_us, sample.cpu_us
        );
    }

    /// Manual baseline (2026-08-12, release, nine runs): filtered batch 128
    /// completed end-to-end in 11,593 us with 128 candidates/one subprocess;
    /// unpruned batch 128 took 22,149 us with 256 candidates/two subprocesses.
    /// Executes every required bounded batch size on the identical fixture;
    /// each of nine rounds rotates the eight mode/batch cases to avoid a fixed
    /// warm-cache ordering advantage.
    #[test]
    #[ignore]
    fn benchmark_search_content_comparisons() {
        assert!(
            ripgrep_available(),
            "ripgrep is required for this benchmark"
        );
        let fixture = create_benchmark_fixture();
        let spec = benchmark_fixture_spec();
        let mut draft =
            SearchDraft::advanced(fixture.path().to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "report".into();
        draft.content = "needle".into();
        draft.types = EntryKinds::FILES;
        draft.minimum_size = "1".into();
        draft.maximum_size = "64".into();
        draft.modified_after = "1970-01-01".into();
        let filtered_request = draft.compile(true).unwrap();
        let mut unpruned_draft =
            SearchDraft::advanced(fixture.path().to_path_buf(), SearchScope::RecursiveHere);
        unpruned_draft.content = "needle".into();
        let unpruned_request = unpruned_draft.compile(true).unwrap();
        let cases = [
            ("filtered", filtered_request),
            ("unpruned", unpruned_request),
        ];
        let mut all_samples = vec![vec![Vec::<StreamingSample>::new(); 4]; 2];
        for round in 0..BENCHMARK_RUNS {
            for offset in 0..8 {
                let case_index = (round + offset) % 8;
                let mode = case_index / 4;
                let batch = case_index % 4;
                let sample =
                    streaming_content_sample(cases[mode].1.clone(), BENCHMARK_BATCH_SIZES[batch]);
                all_samples[mode][batch].push(sample);
            }
        }

        for (mode, (label, _)) in cases.iter().enumerate() {
            for (batch, batch_size) in BENCHMARK_BATCH_SIZES.iter().copied().enumerate() {
                let samples = &all_samples[mode][batch];
                let sample = median_sample(
                    &samples
                        .iter()
                        .map(|sample| sample.timing)
                        .collect::<Vec<_>>(),
                );
                let representative = samples[BENCHMARK_RUNS / 2];
                let expected_candidates = if *label == "filtered" {
                    spec.rg_candidates
                } else {
                    spec.total_files
                };
                assert_eq!(representative.candidates_passed, expected_candidates);
                assert_eq!(representative.matches, spec.matching_files);
                assert_eq!(representative.retained_results, spec.matching_files);
                assert!(representative.max_batch_paths <= batch_size);
                assert!(representative.max_batch_bytes <= RG_BATCH_MAX_ARG_BYTES);
                eprintln!(
                    "PERF search_streaming mode={label} batch_size={batch_size} wall_us={} cpu_us={} candidates_examined={} candidates_passed={} max_batch_paths={} max_batch_bytes={} subprocesses={} matches={} retained_results={} retained_proxy_bytes={} first_result_us={} completion_us={}",
                    sample.wall_us,
                    sample.cpu_us,
                    representative.candidates_examined,
                    representative.candidates_passed,
                    representative.max_batch_paths,
                    representative.max_batch_bytes,
                    representative.subprocesses,
                    representative.matches,
                    representative.retained_results,
                    representative.retained_results * std::mem::size_of::<SearchHit>(),
                    sample.first_us,
                    sample.wall_us
                );
                if *label == "filtered" && batch_size == RG_BATCH_MAX_PATHS {
                    eprintln!(
                        "PERF search_filtered_candidates={} search_filtered_complete_us={}",
                        representative.candidates_passed, sample.wall_us
                    );
                    eprintln!(
                        "PERF search_content_batches={} search_content_complete_us={}",
                        representative.subprocesses, sample.wall_us
                    );
                }
            }
        }

        let unpruned = (0..spec.total_files)
            .map(|index| {
                fixture
                    .path()
                    .join(format!("group-{:02}", index % 16))
                    .join(if index < spec.rg_candidates {
                        format!("report-{index:04}.txt")
                    } else {
                        format!("other-{index:04}.txt")
                    })
            })
            .collect::<Vec<_>>();
        for batch_size in BENCHMARK_BATCH_SIZES {
            let mut samples = Vec::with_capacity(BENCHMARK_RUNS);
            for _ in 0..BENCHMARK_RUNS {
                samples.push(cancellation_sample(&cases[1].1, &unpruned, batch_size));
            }
            let cancel_us = median(samples);
            eprintln!(
                "PERF search_cancel batch_size={batch_size} cancel_to_reaped_us={cancel_us} candidates_in_child={batch_size}"
            );
        }

        let real_cancel_root = fixture.path().join("real-rg-cancel");
        fs::create_dir(&real_cancel_root).unwrap();
        let contents = vec![b'x'; 512 * 1024];
        for index in 0..RG_BATCH_MAX_PATHS {
            fs::write(
                real_cancel_root.join(format!("large-{index:03}.txt")),
                &contents,
            )
            .unwrap();
        }
        let mut real_cancel_draft =
            SearchDraft::advanced(real_cancel_root, SearchScope::RecursiveHere);
        real_cancel_draft.content = "needle-not-present".into();
        let real_cancel_request = real_cancel_draft.compile(true).unwrap();
        let mut real_cancel_samples = Vec::with_capacity(BENCHMARK_RUNS);
        for _ in 0..BENCHMARK_RUNS {
            real_cancel_samples.push(real_rg_cancellation_sample(real_cancel_request.clone()));
        }
        eprintln!(
            "PERF search_cancel_real_rg batch_size=128 cancel_to_finished_us={} fixture_bytes={} child_cleanup=verified_where_observable",
            median(real_cancel_samples),
            contents.len() * RG_BATCH_MAX_PATHS
        );
    }

    fn retention_sample(root: &Path, limit: usize) -> (BenchmarkSample, usize) {
        let mut draft = SearchDraft::advanced(root.to_path_buf(), SearchScope::RecursiveHere);
        draft.name = "result".into();
        let mut request = draft.compile(true).unwrap();
        request.result_limit_override = Some(limit);
        let cpu = process_cpu_us();
        let started = std::time::Instant::now();
        let (hits, completion) = run_traversal(request);
        assert_eq!(completion.truncated, hits.len() == limit);
        (
            BenchmarkSample {
                wall_us: started.elapsed().as_micros(),
                cpu_us: process_cpu_us().saturating_sub(cpu),
                first_us: 0,
            },
            hits.len(),
        )
    }

    /// Manual baseline (2026-08-12, release, nine runs): production-bounded
    /// 1,000 results used a 24,000-byte retained-structure proxy and completed
    /// in 4,268 us versus 480,000 bytes/82,722 us for real collect-all traversal.
    /// Contrasts the production 1,000 cap with a test-only collect-all limit.
    #[test]
    #[ignore]
    fn benchmark_search_streaming_retention_comparison() {
        let fixture = create_benchmark_fixture();
        let retention_root = fixture.path().join("retention");
        fs::create_dir(&retention_root).unwrap();
        for index in 0..benchmark_fixture_spec().retention_results {
            fs::write(retention_root.join(format!("result-{index:05}")), []).unwrap();
        }
        let mut bounded_samples = Vec::new();
        let mut collected_samples = Vec::new();
        let mut bounded_retained = 0;
        let mut collected_retained = 0;
        for _ in 0..BENCHMARK_RUNS {
            let (sample, retained) =
                retention_sample(&retention_root, ResultLimit::OneThousand.get());
            bounded_samples.push(sample);
            bounded_retained = retained;
            let (sample, retained) = retention_sample(
                &retention_root,
                benchmark_fixture_spec().retention_results + 1,
            );
            collected_samples.push(sample);
            collected_retained = retained;
        }
        let bounded = median_sample(&bounded_samples);
        let collected = median_sample(&collected_samples);
        for (mode, sample, retained) in [
            ("bounded", bounded, bounded_retained),
            ("collect_all", collected, collected_retained),
        ] {
            let retained_proxy_bytes = retained * std::mem::size_of::<PathBuf>();
            eprintln!(
                "PERF search_retention mode={mode} wall_us={} cpu_us={} retained_results={retained} retained_proxy_bytes={retained_proxy_bytes} first_result_us=0 completion_us={}",
                sample.wall_us, sample.cpu_us, sample.wall_us
            );
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
