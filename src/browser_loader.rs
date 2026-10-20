use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    app::BrowserView,
    config::UiConfig,
    entry::{self, EntryKind, FileEntry},
};

const UPDATE_QUEUE_CAPACITY: usize = 8;
const BATCH_SIZE: usize = 256;

#[derive(Clone)]
pub struct LoadRequest {
    pub generation: u64,
    pub root: PathBuf,
    pub view: BrowserView,
    pub ui: UiConfig,
    pub expanded: std::collections::HashSet<PathBuf>,
    pub query: Option<String>,
    pub marked: std::collections::HashSet<PathBuf>,
    pub preferred: Option<PathBuf>,
    pub fallback_cursor: usize,
}

pub struct LoadResult {
    pub root: PathBuf,
    pub view: BrowserView,
    pub entries: Vec<FileEntry>,
    pub depths: Vec<usize>,
    pub preferred: Option<PathBuf>,
    pub fallback_cursor: usize,
    pub warning: Option<String>,
    pub elapsed: Duration,
}

pub enum LoadUpdate {
    Batch {
        generation: u64,
        entries: Vec<FileEntry>,
        depths: Vec<usize>,
    },
    Finished {
        generation: u64,
        result: Result<LoadResult, String>,
    },
}

pub struct RunningLoad {
    pub generation: u64,
    pub receiver: Receiver<LoadUpdate>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for RunningLoad {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(request: LoadRequest) -> RunningLoad {
    let generation = request.generation;
    let (sender, receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || run(request, sender, worker_cancel));
    RunningLoad {
        generation,
        receiver,
        cancel,
    }
}

fn run(request: LoadRequest, sender: SyncSender<LoadUpdate>, cancel: Arc<AtomicBool>) {
    let started = Instant::now();
    let result = load(&request, &sender, &cancel).map(|mut result| {
        result.elapsed = started.elapsed();
        result
    });
    let _ = sender.send(LoadUpdate::Finished {
        generation: request.generation,
        result,
    });
}

fn load(
    request: &LoadRequest,
    sender: &SyncSender<LoadUpdate>,
    cancel: &AtomicBool,
) -> Result<LoadResult, String> {
    let mut entries = Vec::new();
    let mut depths = Vec::new();
    let mut warning = None;

    append_directory(
        &request.root,
        0,
        request,
        sender,
        cancel,
        &mut entries,
        &mut depths,
        &mut warning,
        true,
    )?;

    if cancel.load(Ordering::Relaxed) {
        return Err("directory load cancelled".into());
    }
    Ok(LoadResult {
        root: request.root.clone(),
        view: request.view,
        entries,
        depths,
        preferred: request.preferred.clone(),
        fallback_cursor: request.fallback_cursor,
        warning,
        elapsed: Duration::ZERO,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_directory(
    path: &Path,
    depth: usize,
    request: &LoadRequest,
    sender: &SyncSender<LoadUpdate>,
    cancel: &AtomicBool,
    entries: &mut Vec<FileEntry>,
    depths: &mut Vec<usize>,
    warning: &mut Option<String>,
    root: bool,
) -> Result<(), String> {
    let mut children = match read_children(path, depth, request, sender, cancel) {
        Ok(children) => children,
        Err(error) if !root => {
            warning.get_or_insert(error);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    entry::sort_entries(
        &mut children,
        request.ui.sort,
        request.ui.reverse_sort,
        request.ui.directories_first,
    );

    for mut child in children {
        if cancel.load(Ordering::Relaxed) {
            return Err("directory load cancelled".into());
        }
        let recurse = request.view == BrowserView::Tree
            && child.kind == EntryKind::Directory
            && request.expanded.contains(&child.path);
        let child_path = child.path.clone();
        child.selected = request.marked.contains(&child.path);
        let shown_depth = if request.query.is_some() { 0 } else { depth };
        entries.push(child);
        depths.push(shown_depth);
        if recurse {
            append_directory(
                &child_path,
                depth + 1,
                request,
                sender,
                cancel,
                entries,
                depths,
                warning,
                false,
            )?;
        }
    }
    Ok(())
}

fn read_children(
    path: &Path,
    depth: usize,
    request: &LoadRequest,
    sender: &SyncSender<LoadUpdate>,
    cancel: &AtomicBool,
) -> Result<Vec<FileEntry>, String> {
    let read = fs::read_dir(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut entries = Vec::new();
    let mut batch_entries = Vec::with_capacity(BATCH_SIZE);
    let mut batch_depths = Vec::with_capacity(BATCH_SIZE);
    let mut streaming = true;
    for item in read {
        if cancel.load(Ordering::Relaxed) {
            return Err("directory load cancelled".into());
        }
        let Ok(item) = item else { continue };
        let Ok(mut entry) = FileEntry::from_dir_entry(item) else {
            continue;
        };
        if !request.ui.show_hidden && entry.is_hidden() {
            continue;
        }
        if request
            .query
            .as_deref()
            .is_some_and(|query| !entry::contains_case_insensitive(&entry.name, query))
        {
            continue;
        }
        entry.selected = request.marked.contains(&entry.path);
        if streaming {
            batch_entries.push(entry.clone());
            batch_depths.push(if request.query.is_some() { 0 } else { depth });
        }
        if streaming && batch_entries.len() >= BATCH_SIZE {
            streaming = flush_batch(
                request.generation,
                sender,
                &mut batch_entries,
                &mut batch_depths,
            );
        }
        entries.push(entry);
    }
    if streaming {
        flush_batch(
            request.generation,
            sender,
            &mut batch_entries,
            &mut batch_depths,
        );
    }
    Ok(entries)
}

fn flush_batch(
    generation: u64,
    sender: &SyncSender<LoadUpdate>,
    entries: &mut Vec<FileEntry>,
    depths: &mut Vec<usize>,
) -> bool {
    if entries.is_empty() {
        return true;
    }
    let update = LoadUpdate::Batch {
        generation,
        entries: std::mem::take(entries),
        depths: std::mem::take(depths),
    };
    match sender.try_send(update) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::atomic::AtomicBool, time::Instant};

    use tempfile::tempdir;

    use super::*;

    fn request(root: PathBuf, generation: u64) -> LoadRequest {
        LoadRequest {
            generation,
            root,
            view: BrowserView::Table,
            ui: UiConfig::default(),
            expanded: Default::default(),
            query: None,
            marked: Default::default(),
            preferred: None,
            fallback_cursor: 0,
        }
    }

    #[test]
    fn loader_streams_and_finishes_with_a_sorted_snapshot() {
        let temp = tempdir().unwrap();
        for index in (0..600).rev() {
            fs::write(temp.path().join(format!("item-{index:04}")), []).unwrap();
        }
        let (sender, receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
        let cancel = AtomicBool::new(false);
        let result = load(&request(temp.path().to_path_buf(), 7), &sender, &cancel).unwrap();
        assert_eq!(result.entries.len(), 600);
        assert_eq!(result.entries[0].name, "item-0000");
        assert!(matches!(
            receiver.try_recv(),
            Ok(LoadUpdate::Batch { generation: 7, .. })
        ));
    }

    #[test]
    fn cancelled_load_does_not_return_a_snapshot() {
        let temp = tempdir().unwrap();
        let (sender, _receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
        let cancel = AtomicBool::new(true);
        let result = load(&request(temp.path().to_path_buf(), 1), &sender, &cancel);
        assert!(result.is_err());
    }

    #[test]
    #[ignore]
    fn benchmark_background_directory_load() {
        let root = std::env::var_os("MINFM_PERF_LARGE_DIR")
            .map(PathBuf::from)
            .expect("MINFM_PERF_LARGE_DIR is required");
        let mut first_samples = Vec::new();
        let mut total_samples = Vec::new();
        let mut enqueue_samples = Vec::new();
        let mut synchronous_samples = Vec::new();
        for generation in 0..9 {
            let synchronous_started = Instant::now();
            let synchronous =
                entry::read_directory(&root, false, crate::config::SortSetting::Name, false, true)
                    .unwrap();
            assert_eq!(synchronous.len(), 20_000);
            synchronous_samples.push(synchronous_started.elapsed());

            let started = Instant::now();
            let running = spawn(request(root.clone(), generation));
            enqueue_samples.push(started.elapsed());
            let mut first_batch = None;
            loop {
                match running.receiver.recv().unwrap() {
                    LoadUpdate::Batch { .. } => {
                        first_batch.get_or_insert_with(|| started.elapsed());
                    }
                    LoadUpdate::Finished { result, .. } => {
                        let result = result.unwrap();
                        assert_eq!(result.entries.len(), 20_000);
                        first_samples.push(first_batch.unwrap_or_default());
                        total_samples.push(started.elapsed());
                        break;
                    }
                }
            }
        }
        first_samples.sort();
        total_samples.sort();
        enqueue_samples.sort();
        synchronous_samples.sort();
        println!(
            "PERF synchronous_median_us={} background_enqueue_median_us={} background_first_batch_median_us={} background_total_median_us={}",
            synchronous_samples[4].as_micros(),
            enqueue_samples[4].as_micros(),
            first_samples[4].as_micros(),
            total_samples[4].as_micros(),
        );
    }
}
