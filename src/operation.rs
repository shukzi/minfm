use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
};

use filetime::FileTime;
use nix::fcntl::{Flock, FlockArg};

use crate::{
    error::{io_error, MinfmError, Result},
    safety,
    trash::{TrashEntry, TrashManager},
};

#[derive(Debug)]
struct MetadataPreservation {
    permissions: bool,
    timestamps: bool,
    xattrs: bool,
}

impl Default for MetadataPreservation {
    fn default() -> Self {
        Self {
            permissions: true,
            timestamps: true,
            xattrs: true,
        }
    }
}

impl MetadataPreservation {
    fn skip_permissions(&mut self, warnings: &mut Vec<String>) {
        if self.permissions {
            self.permissions = false;
            push_warning_once(
                warnings,
                "destination filesystem cannot preserve Unix permissions exactly; that metadata was skipped"
                    .into(),
            );
        }
    }

    fn skip_timestamps(&mut self, warnings: &mut Vec<String>) {
        if self.timestamps {
            self.timestamps = false;
            push_warning_once(
                warnings,
                "destination filesystem cannot preserve timestamps exactly; that metadata was skipped"
                    .into(),
            );
        }
    }

    fn skip_xattrs(&mut self, warnings: &mut Vec<String>) {
        if self.xattrs {
            self.xattrs = false;
            push_warning_once(
                warnings,
                "extended attributes are unsupported by one side of this transfer; that metadata was skipped"
                    .into(),
            );
        }
    }
}

fn push_warning_once(warnings: &mut Vec<String>, warning: String) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum XattrMatch {
    Match,
    Mismatch,
    Unsupported,
}

#[derive(Debug, Clone)]
pub enum OperationRequest {
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        cut: bool,
        overwrite: bool,
        verify: bool,
        current_dir: PathBuf,
        config_dir: PathBuf,
    },
    Trash {
        paths: Vec<PathBuf>,
        current_dir: PathBuf,
        config_dir: PathBuf,
    },
    PermanentlyDelete {
        entries: Vec<TrashEntry>,
        manager: TrashManager,
    },
}

#[derive(Debug, Clone, Default)]
pub struct OperationSummary {
    pub label: String,
    pub completed: usize,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum OperationUpdate {
    Started {
        label: String,
        total_items: usize,
        total_bytes: u64,
    },
    Progress {
        current: PathBuf,
        completed_items: usize,
        completed_bytes: u64,
    },
    Finished(OperationSummary),
}

pub struct RunningOperation {
    pub receiver: Receiver<OperationUpdate>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(request: OperationRequest) -> RunningOperation {
    const UPDATE_QUEUE_CAPACITY: usize = 256;
    let (sender, receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || run(request, sender, worker_cancel));
    RunningOperation { receiver, cancel }
}

fn run(request: OperationRequest, sender: SyncSender<OperationUpdate>, cancel: Arc<AtomicBool>) {
    let (label, paths) = match &request {
        OperationRequest::Copy { sources, cut, .. } => (
            if *cut { "Moving" } else { "Copying" }.to_string(),
            sources.clone(),
        ),
        OperationRequest::Trash { paths, .. } => ("Moving to trash".to_string(), paths.clone()),
        OperationRequest::PermanentlyDelete { entries, .. } => (
            "Permanently deleting".to_string(),
            entries
                .iter()
                .map(|entry| entry.trashed_path.clone())
                .collect(),
        ),
    };
    let paths = paths
        .into_iter()
        .map(|path| {
            let estimated_size = estimate_size(&path);
            (path, estimated_size)
        })
        .collect::<Vec<_>>();
    let total_bytes = paths.iter().map(|(_, size)| size).sum();
    let _ = sender.send(OperationUpdate::Started {
        label: label.clone(),
        total_items: paths.len(),
        total_bytes,
    });
    let mut summary = OperationSummary {
        label,
        ..Default::default()
    };
    let mut completed_bytes: u64 = 0;

    for (path, path_bytes) in paths {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let result = match &request {
            OperationRequest::Copy {
                destination,
                cut,
                overwrite,
                verify,
                current_dir,
                config_dir,
                ..
            } => copy_or_move(
                &path,
                destination,
                *cut,
                *overwrite,
                *verify,
                current_dir,
                config_dir,
                &cancel,
                &mut summary.warnings,
            ),
            OperationRequest::Trash {
                current_dir,
                config_dir,
                ..
            } => TrashManager::for_path(&path).and_then(|trash| {
                trash
                    .move_to_trash(&path, current_dir, config_dir)
                    .map(|_| ())
            }),
            OperationRequest::PermanentlyDelete { entries, manager } => entries
                .iter()
                .find(|entry| entry.trashed_path == path)
                .ok_or_else(|| MinfmError::Message("trash entry disappeared".into()))
                .and_then(|entry| manager.permanently_delete(entry)),
        };
        match result {
            Ok(()) => {
                summary.completed += 1;
                completed_bytes = completed_bytes.saturating_add(path_bytes);
            }
            Err(MinfmError::Cancelled) => {
                summary.cancelled = true;
                break;
            }
            Err(error) => summary.failed.push((path.clone(), error.to_string())),
        }
        let _ = sender.send(OperationUpdate::Progress {
            current: path,
            completed_items: summary.completed,
            completed_bytes,
        });
    }
    let _ = sender.send(OperationUpdate::Finished(summary));
}

#[allow(clippy::too_many_arguments)]
fn copy_or_move(
    source: &Path,
    destination_dir: &Path,
    cut: bool,
    overwrite: bool,
    verify: bool,
    current_dir: &Path,
    config_dir: &Path,
    cancel: &AtomicBool,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let name = source
        .file_name()
        .ok_or_else(|| MinfmError::Message(format!("{} has no filename", source.display())))?;
    let destination = destination_dir.join(name);
    safety::ensure_no_overlap(source, &destination)?;
    let can_rename = cut && same_filesystem(source, destination_dir)?;

    let overwritten: Option<(TrashManager, TrashEntry)> = if destination.exists() {
        if !overwrite {
            return Err(MinfmError::DestinationExists(destination));
        }
        let trash = TrashManager::for_path(&destination)?;
        let entry = trash.move_to_trash(&destination, current_dir, config_dir)?;
        Some((trash, entry))
    } else {
        None
    };

    let result = if can_rename {
        fs::rename(source, &destination).map_err(|error| {
            io_error(
                format!(
                    "could not move {} to {}",
                    source.display(),
                    destination.display()
                ),
                error,
            )
        })
    } else {
        copy_safely(
            source,
            &destination,
            verification_required(verify, cut),
            cancel,
            warnings,
        )
        .and_then(|()| {
            if cut {
                trash_source_after_verified_copy(source, &destination, current_dir, config_dir)?;
            }
            Ok(())
        })
    };
    if let Err(error) = result {
        if let Some((trash, entry)) = overwritten {
            if destination.exists() {
                let replacement_trash = TrashManager::for_path(&destination);
                let replacement_result = replacement_trash.and_then(|manager| {
                    manager
                        .move_to_trash(&destination, current_dir, config_dir)
                        .map(|_| ())
                });
                if let Err(rollback_error) = replacement_result {
                    return Err(MinfmError::Message(format!(
                        "{error}; the previous destination is safe in trash, but the incomplete replacement could not be moved aside: {rollback_error}"
                    )));
                }
            }
            if let Err(restore_error) = trash.restore(&entry, None) {
                return Err(MinfmError::Message(format!(
                    "{error}; the previous destination is safe in trash but automatic restore failed: {restore_error}"
                )));
            }
        }
        return Err(error);
    }
    Ok(())
}

fn verification_required(configured: bool, cut: bool) -> bool {
    configured || cut
}

fn trash_source_after_verified_copy(
    source: &Path,
    destination: &Path,
    current_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let trash_result = safety::ensure_trashable(source, current_dir, config_dir).and_then(|()| {
        TrashManager::for_path(source).and_then(|trash| {
            trash
                .move_to_trash(source, current_dir, config_dir)
                .map(|_| ())
        })
    });
    trash_result.map_err(|error| {
        MinfmError::Message(format!(
            "copy verified at {}, but the source was retained because it could not be moved to trash: {error}",
            destination.display()
        ))
    })
}

fn copy_safely(
    source: &Path,
    destination: &Path,
    verify: bool,
    cancel: &AtomicBool,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if destination.exists() {
        return Err(MinfmError::DestinationExists(destination.to_path_buf()));
    }
    let parent = destination.parent().ok_or_else(|| {
        MinfmError::Message(format!(
            "{} has no destination directory",
            destination.display()
        ))
    })?;
    let name = destination.file_name().unwrap_or_default();
    let _copy_lock = CopyLock::acquire(parent, name)?;
    let recovered = recover_stale_partials(parent, name)?;
    if recovered > 0 {
        warnings.push(format!(
            "recovered {recovered} incomplete prior copy artifact(s) for {}",
            destination.display()
        ));
    }
    let temporary = unique_partial_path(parent, name);
    let mut preservation = MetadataPreservation::default();
    let result = copy_path(source, &temporary, cancel, warnings, &mut preservation);
    if let Err(error) = result {
        cleanup_partial(&temporary);
        return Err(error);
    }
    if verify && !verify_tree(source, &temporary, &mut preservation, warnings)? {
        cleanup_partial(&temporary);
        return Err(MinfmError::Message(format!(
            "verification failed while copying {}",
            source.display()
        )));
    }
    finalize_no_replace(&temporary, destination).map_err(|error| {
        cleanup_partial(&temporary);
        io_error(
            format!("could not finalize {}", destination.display()),
            error,
        )
    })?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

struct CopyLock {
    lock: Option<Flock<File>>,
    path: PathBuf,
}

impl CopyLock {
    fn acquire(parent: &Path, name: &OsStr) -> Result<Self> {
        let path = copy_lock_path(parent, name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| {
                io_error(
                    format!("could not open copy lock {}", path.display()),
                    error,
                )
            })?;
        let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
            MinfmError::Message(format!(
                "another copy to this destination is already active: {error}"
            ))
        })?;
        Ok(Self {
            lock: Some(lock),
            path,
        })
    }
}

impl Drop for CopyLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        drop(self.lock.take());
    }
}

fn copy_lock_path(parent: &Path, name: &OsStr) -> PathBuf {
    let mut bytes = Vec::with_capacity(name.as_bytes().len() + 18);
    bytes.push(b'.');
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(b".minfm-copy.lock");
    parent.join(OsString::from_vec(bytes))
}

fn partial_prefix(name: &OsStr) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(name.as_bytes().len() + 16);
    prefix.push(b'.');
    prefix.extend_from_slice(name.as_bytes());
    prefix.extend_from_slice(b".minfm-partial-");
    prefix
}

fn recover_stale_partials(parent: &Path, name: &OsStr) -> Result<usize> {
    let prefix = partial_prefix(name);
    let mut recovered = 0;
    for entry in fs::read_dir(parent)
        .map_err(|error| io_error(format!("could not inspect {}", parent.display()), error))?
    {
        let entry = entry.map_err(|error| io_error("could not inspect copy artifacts", error))?;
        if !entry.file_name().as_bytes().starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error(format!("could not inspect {}", path.display()), error))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path).map_err(|error| {
                io_error(
                    format!("could not recover incomplete copy {}", path.display()),
                    error,
                )
            })?;
        } else {
            fs::remove_file(&path).map_err(|error| {
                io_error(
                    format!("could not recover incomplete copy {}", path.display()),
                    error,
                )
            })?;
        }
        recovered += 1;
    }
    Ok(recovered)
}

fn copy_path(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    warnings: &mut Vec<String>,
    preservation: &mut MetadataPreservation,
) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(MinfmError::Cancelled);
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| io_error(format!("could not inspect {}", source.display()), error))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(|error| {
            io_error(format!("could not read link {}", source.display()), error)
        })?;
        symlink(target, destination).map_err(|error| {
            io_error(format!("could not copy link {}", source.display()), error)
        })?;
        preserve_xattrs(source, destination, warnings, preservation);
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir(destination).map_err(|error| {
            io_error(format!("could not create {}", destination.display()), error)
        })?;
        for item in fs::read_dir(source)
            .map_err(|error| io_error(format!("could not read {}", source.display()), error))?
        {
            let item = item.map_err(|error| io_error("could not read directory entry", error))?;
            copy_path(
                &item.path(),
                &destination.join(item.file_name()),
                cancel,
                warnings,
                preservation,
            )?;
        }
        preserve_metadata(source, &metadata, destination, warnings, preservation);
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(MinfmError::Message(format!(
            "unsupported file type: {}",
            source.display()
        )));
    }

    let mut input = File::open(source)
        .map_err(|error| io_error(format!("could not open {}", source.display()), error))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| io_error(format!("could not create {}", destination.display()), error))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MinfmError::Cancelled);
        }
        let read = input
            .read(&mut buffer)
            .map_err(|error| io_error(format!("could not read {}", source.display()), error))?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|error| {
            io_error(format!("could not write {}", destination.display()), error)
        })?;
    }
    output
        .sync_all()
        .map_err(|error| io_error(format!("could not flush {}", destination.display()), error))?;
    preserve_metadata(source, &metadata, destination, warnings, preservation);
    Ok(())
}

fn preserve_metadata(
    source: &Path,
    metadata: &fs::Metadata,
    destination: &Path,
    warnings: &mut Vec<String>,
    preservation: &mut MetadataPreservation,
) {
    preserve_xattrs(source, destination, warnings, preservation);
    if preservation.permissions {
        let expected_mode = metadata.mode() & 0o7777;
        let permissions = fs::Permissions::from_mode(expected_mode);
        let preserved = fs::set_permissions(destination, permissions)
            .and_then(|()| fs::metadata(destination))
            .map(|actual| actual.mode() & 0o7777 == expected_mode)
            .unwrap_or(false);
        if !preserved {
            preservation.skip_permissions(warnings);
        }
    }
    if preservation.timestamps {
        let atime = FileTime::from_last_access_time(metadata);
        let mtime = FileTime::from_last_modification_time(metadata);
        let preserved = filetime::set_file_times(destination, atime, mtime)
            .and_then(|()| fs::metadata(destination))
            .map(|actual| FileTime::from_last_modification_time(&actual) == mtime)
            .unwrap_or(false);
        if !preserved {
            preservation.skip_timestamps(warnings);
        }
    }
}

fn preserve_xattrs(
    source: &Path,
    destination: &Path,
    warnings: &mut Vec<String>,
    preservation: &mut MetadataPreservation,
) {
    if !preservation.xattrs {
        return;
    }
    let attributes = match xattr::list(source) {
        Ok(attributes) => attributes
            .filter(|name| should_verify_xattr(name))
            .collect::<Vec<_>>(),
        Err(error) if is_unsupported_xattr_error(&error) => {
            preservation.skip_xattrs(warnings);
            return;
        }
        Err(error) => {
            warnings.push(format!(
                "could not list extended attributes for {}: {error}",
                source.display()
            ));
            return;
        }
    };
    for name in attributes {
        match xattr::get(source, &name) {
            Ok(Some(value)) => {
                if let Err(error) = xattr::set(destination, &name, &value) {
                    if is_unsupported_xattr_error(&error) {
                        preservation.skip_xattrs(warnings);
                        return;
                    }
                    warnings.push(format!(
                        "could not preserve extended attribute {:?} for {}: {error}",
                        name,
                        destination.display()
                    ));
                }
            }
            Ok(None) => {}
            Err(error) if is_unsupported_xattr_error(&error) => {
                preservation.skip_xattrs(warnings);
                return;
            }
            Err(error) => warnings.push(format!(
                "could not read extended attribute {:?} from {}: {error}",
                name,
                source.display()
            )),
        }
    }
}

fn finalize_no_replace(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        temporary,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

fn verify_tree(
    source: &Path,
    destination: &Path,
    preservation: &mut MetadataPreservation,
    warnings: &mut Vec<String>,
) -> Result<bool> {
    let source_meta = fs::symlink_metadata(source)
        .map_err(|error| io_error(format!("could not verify {}", source.display()), error))?;
    let destination_meta = fs::symlink_metadata(destination)
        .map_err(|error| io_error(format!("could not verify {}", destination.display()), error))?;
    if source_meta.file_type().is_symlink() {
        return Ok(destination_meta.file_type().is_symlink()
            && fs::read_link(source).ok() == fs::read_link(destination).ok()
            && verified_xattrs_match(source, destination, preservation, warnings)?);
    }
    if source_meta.is_file() {
        return Ok(destination_meta.is_file()
            && metadata_matches(&source_meta, &destination_meta, preservation)
            && source_meta.len() == destination_meta.len()
            && verified_xattrs_match(source, destination, preservation, warnings)?
            && files_match(source, destination)?);
    }
    if source_meta.is_dir() && destination_meta.is_dir() {
        if !metadata_matches(&source_meta, &destination_meta, preservation)
            || !verified_xattrs_match(source, destination, preservation, warnings)?
        {
            return Ok(false);
        }
        let mut source_names = std::collections::HashSet::new();
        for item in fs::read_dir(source)
            .map_err(|error| io_error(format!("could not verify {}", source.display()), error))?
        {
            let item = item.map_err(|error| io_error("could not verify directory entry", error))?;
            source_names.insert(item.file_name());
            if !verify_tree(
                &item.path(),
                &destination.join(item.file_name()),
                preservation,
                warnings,
            )? {
                return Ok(false);
            }
        }
        let destination_names = fs::read_dir(destination)
            .map_err(|error| {
                io_error(format!("could not verify {}", destination.display()), error)
            })?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::result::Result<std::collections::HashSet<_>, _>>()
            .map_err(|error| io_error("could not verify destination directory", error))?;
        if source_names != destination_names {
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(false)
}

fn verified_xattrs_match(
    source: &Path,
    destination: &Path,
    preservation: &mut MetadataPreservation,
    warnings: &mut Vec<String>,
) -> Result<bool> {
    if !preservation.xattrs {
        return Ok(true);
    }
    match xattrs_match(source, destination)? {
        XattrMatch::Match => Ok(true),
        XattrMatch::Mismatch => Ok(false),
        XattrMatch::Unsupported => {
            preservation.skip_xattrs(warnings);
            Ok(true)
        }
    }
}

fn xattrs_match(source: &Path, destination: &Path) -> Result<XattrMatch> {
    xattrs_match_with(
        source,
        destination,
        |path| {
            xattr::list(path).map(|attributes| {
                attributes
                    .filter(|name| should_verify_xattr(name))
                    .collect()
            })
        },
        |path, name| xattr::get(path, name),
    )
}

fn xattrs_match_with<List, Get>(
    source: &Path,
    destination: &Path,
    mut list: List,
    mut get: Get,
) -> Result<XattrMatch>
where
    List: FnMut(&Path) -> std::io::Result<HashSet<OsString>>,
    Get: FnMut(&Path, &OsStr) -> std::io::Result<Option<Vec<u8>>>,
{
    let source_names = match list(source) {
        Ok(names) => names,
        Err(error) if is_unsupported_xattr_error(&error) => return Ok(XattrMatch::Unsupported),
        Err(error) => {
            return Err(io_error(
                format!("could not verify xattrs for {}", source.display()),
                error,
            ));
        }
    };
    let destination_names = match list(destination) {
        Ok(names) => names,
        Err(error) if is_unsupported_xattr_error(&error) => return Ok(XattrMatch::Unsupported),
        Err(error) => {
            return Err(io_error(
                format!("could not verify xattrs for {}", destination.display()),
                error,
            ));
        }
    };
    if source_names != destination_names {
        return Ok(XattrMatch::Mismatch);
    }
    for name in source_names {
        let source_value = match get(source, &name) {
            Ok(value) => value,
            Err(error) if is_unsupported_xattr_error(&error) => {
                return Ok(XattrMatch::Unsupported);
            }
            Err(error) => {
                return Err(io_error(
                    format!("could not verify xattr {:?} for {}", name, source.display()),
                    error,
                ));
            }
        };
        let destination_value = match get(destination, &name) {
            Ok(value) => value,
            Err(error) if is_unsupported_xattr_error(&error) => {
                return Ok(XattrMatch::Unsupported);
            }
            Err(error) => {
                return Err(io_error(
                    format!(
                        "could not verify xattr {:?} for {}",
                        name,
                        destination.display()
                    ),
                    error,
                ));
            }
        };
        if source_value != destination_value {
            return Ok(XattrMatch::Mismatch);
        }
    }
    Ok(XattrMatch::Match)
}

fn is_unsupported_xattr_error(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(nix::libc::EOPNOTSUPP)
}

fn should_verify_xattr(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name.starts_with(b"user.") || name.starts_with(b"system.posix_acl_")
}

fn metadata_matches(
    source: &fs::Metadata,
    destination: &fs::Metadata,
    preservation: &MetadataPreservation,
) -> bool {
    source.file_type() == destination.file_type()
        && (!preservation.permissions || source.mode() & 0o7777 == destination.mode() & 0o7777)
        && (!preservation.timestamps
            || FileTime::from_last_modification_time(source)
                == FileTime::from_last_modification_time(destination))
}

fn files_match(left: &Path, right: &Path) -> Result<bool> {
    let mut left = File::open(left)
        .map_err(|error| io_error("could not open source for verification", error))?;
    let mut right = File::open(right)
        .map_err(|error| io_error("could not open destination for verification", error))?;
    let mut left_buffer = vec![0u8; 1024 * 1024];
    let mut right_buffer = vec![0u8; 1024 * 1024];
    loop {
        let left_read = left
            .read(&mut left_buffer)
            .map_err(|error| io_error("could not verify source bytes", error))?;
        let right_read = right
            .read(&mut right_buffer)
            .map_err(|error| io_error("could not verify destination bytes", error))?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn same_filesystem(source: &Path, destination_dir: &Path) -> Result<bool> {
    let source_dev = fs::metadata(source)
        .map_err(|error| io_error(format!("could not inspect {}", source.display()), error))?
        .dev();
    let destination_dev = fs::metadata(destination_dir)
        .map_err(|error| {
            io_error(
                format!("could not inspect {}", destination_dir.display()),
                error,
            )
        })?
        .dev();
    Ok(source_dev == destination_dev)
}

fn estimate_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if metadata.is_dir() {
        return fs::read_dir(path)
            .map(|read| read.flatten().map(|item| estimate_size(&item.path())).sum())
            .unwrap_or(0);
    }
    0
}

fn unique_partial_path(parent: &Path, name: &OsStr) -> PathBuf {
    let name = name.to_string_lossy();
    for counter in 0u64.. {
        let path = parent.join(format!(".{name}.minfm-partial-{counter}"));
        if !path.exists() {
            return path;
        }
    }
    unreachable!()
}

fn cleanup_partial(path: &Path) {
    if path.is_dir() && !path.is_symlink() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn verify_for_test(source: &Path, destination: &Path) -> bool {
        let mut preservation = MetadataPreservation::default();
        let mut warnings = Vec::new();
        verify_tree(source, destination, &mut preservation, &mut warnings).unwrap()
    }

    #[test]
    #[ignore]
    fn benchmark_operation_size_preflight() {
        let root = std::env::var_os("MINFM_PERF_SEARCH_DIR")
            .map(PathBuf::from)
            .expect("MINFM_PERF_SEARCH_DIR is required");
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            assert_eq!(estimate_size(&root), 0);
            samples.push(started.elapsed());
        }
        samples.sort();
        eprintln!(
            "PERF operation_preflight_median_us={}",
            samples[4].as_micros()
        );
    }

    #[test]
    fn failed_copy_does_not_replace_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();
        assert!(copy_safely(&source, &destination, true, &cancel, &mut warnings).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"old");
    }

    #[test]
    fn safe_copy_preserves_bytes_and_mode() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"exact bytes").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();
        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"exact bytes");
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn safe_copy_preserves_and_verifies_extended_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"exact bytes").unwrap();
        xattr::set(&source, "user.minfm-test", b"preserved").unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();

        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(
            xattr::get(&destination, "user.minfm-test").unwrap(),
            Some(b"preserved".to_vec())
        );
        assert!(verify_for_test(&source, &destination));

        xattr::set(&destination, "user.minfm-test", b"changed").unwrap();
        assert!(!verify_for_test(&source, &destination));
    }

    #[test]
    fn unsupported_xattrs_do_not_fail_content_verification() {
        let source = Path::new("source");
        let destination = Path::new("destination");
        let result = xattrs_match_with(
            source,
            destination,
            |_| Err(std::io::Error::from_raw_os_error(nix::libc::EOPNOTSUPP)),
            |_, _| -> std::io::Result<Option<Vec<u8>>> {
                panic!("values must not be read when xattrs are unsupported")
            },
        )
        .unwrap();

        assert_eq!(result, XattrMatch::Unsupported);
    }

    #[test]
    fn repeated_unsupported_metadata_has_one_operation_warning() {
        let mut warnings = Vec::new();
        MetadataPreservation::default().skip_xattrs(&mut warnings);
        MetadataPreservation::default().skip_xattrs(&mut warnings);

        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn unexpected_xattr_errors_remain_fatal() {
        let result = xattrs_match_with(
            Path::new("source"),
            Path::new("destination"),
            |_| Err(std::io::Error::from_raw_os_error(nix::libc::EIO)),
            |_, _| -> std::io::Result<Option<Vec<u8>>> { unreachable!() },
        );

        assert!(
            matches!(result, Err(MinfmError::Io { source, .. }) if source.raw_os_error() == Some(nix::libc::EIO))
        );
    }

    #[test]
    fn unavailable_metadata_does_not_weaken_byte_verification() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::write(&source, b"matching bytes").unwrap();
        fs::write(&destination, b"matching bytes").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
        let mut preservation = MetadataPreservation {
            permissions: false,
            timestamps: false,
            xattrs: false,
        };
        let mut warnings = Vec::new();

        assert!(verify_tree(&source, &destination, &mut preservation, &mut warnings,).unwrap());

        fs::write(&destination, b"different bytes").unwrap();
        assert!(!verify_tree(&source, &destination, &mut preservation, &mut warnings,).unwrap());
    }

    #[test]
    fn failed_post_copy_trash_keeps_source_and_reports_verified_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(&destination, b"verified copy").unwrap();

        let error = trash_source_after_verified_copy(&source, &destination, &source, temp.path())
            .unwrap_err()
            .to_string();

        assert!(source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"verified copy");
        assert!(error.contains("copy verified"));
        assert!(error.contains("source was retained"));
    }

    #[test]
    fn cross_filesystem_moves_always_require_verification() {
        assert!(verification_required(false, true));
        assert!(verification_required(true, true));
        assert!(verification_required(true, false));
        assert!(!verification_required(false, false));
    }

    #[test]
    fn safe_copy_supports_non_utf8_filenames() {
        let temp = tempfile::tempdir().unwrap();
        let source_name = OsString::from_vec(b"source-\xff.bin".to_vec());
        let destination_name = OsString::from_vec(b"destination-\xfe.bin".to_vec());
        let source = temp.path().join(source_name);
        let destination = temp.path().join(destination_name);
        fs::write(&source, b"non-utf8 name bytes").unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();

        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(fs::read(destination).unwrap(), b"non-utf8 name bytes");
    }

    #[test]
    fn finalization_never_replaces_a_racing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let partial = temp.path().join(".partial");
        let destination = temp.path().join("destination");
        fs::write(&partial, b"new data").unwrap();
        fs::write(&destination, b"existing data").unwrap();

        assert!(finalize_no_replace(&partial, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"existing data");
        assert_eq!(fs::read(&partial).unwrap(), b"new data");
    }

    #[test]
    fn symlink_copy_preserves_the_link_without_following_it() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let source = temp.path().join("source-link");
        let destination = temp.path().join("destination-link");
        fs::write(&target, b"target bytes").unwrap();
        symlink("target", &source).unwrap();

        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();
        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();

        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(destination).unwrap(), PathBuf::from("target"));
    }

    #[test]
    fn cancelled_copy_removes_partial_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, vec![0x5a; 2 * 1024 * 1024]).unwrap();
        let cancel = AtomicBool::new(true);
        let mut warnings = Vec::new();

        assert!(matches!(
            copy_safely(&source, &destination, true, &cancel, &mut warnings),
            Err(MinfmError::Cancelled)
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn interrupted_copy_artifacts_are_recovered_under_a_destination_lock() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let stale_partial = temp.path().join(".destination.bin.minfm-partial-0");
        let stale_lock = copy_lock_path(temp.path(), OsStr::new("destination.bin"));
        fs::write(&source, b"complete source").unwrap();
        fs::write(&stale_partial, b"interrupted bytes").unwrap();
        fs::write(&stale_lock, b"").unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();

        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"complete source");
        assert!(!stale_partial.exists());
        assert!(!stale_lock.exists());
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("recovered 1 incomplete prior copy")));
    }

    #[test]
    fn concurrent_copy_to_the_same_destination_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"source").unwrap();
        let lock = CopyLock::acquire(temp.path(), OsStr::new("destination.bin")).unwrap();
        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();

        let result = copy_safely(&source, &destination, true, &cancel, &mut warnings);

        assert!(
            matches!(result, Err(MinfmError::Message(message)) if message.contains("already active"))
        );
        assert!(!destination.exists());
        drop(lock);
        assert!(!copy_lock_path(temp.path(), OsStr::new("destination.bin")).exists());
    }

    #[test]
    fn verification_rejects_metadata_or_directory_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("one"), b"one").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("one"), b"one").unwrap();
        fs::write(destination.join("unexpected"), b"extra").unwrap();
        assert!(!verify_for_test(&source, &destination));

        fs::remove_file(destination.join("unexpected")).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!verify_for_test(&source, &destination));
    }

    #[test]
    fn stress_copy_verifies_a_nested_tree() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source-tree");
        let destination = temp.path().join("destination-tree");
        fs::create_dir(&source).unwrap();
        for directory_index in 0..20 {
            let directory = source.join(format!("directory-{directory_index}"));
            fs::create_dir(&directory).unwrap();
            for file_index in 0..20 {
                fs::write(
                    directory.join(format!("file-{file_index}")),
                    format!("directory {directory_index}, file {file_index}"),
                )
                .unwrap();
            }
        }

        let cancel = AtomicBool::new(false);
        let mut warnings = Vec::new();
        copy_safely(&source, &destination, true, &cancel, &mut warnings).unwrap();

        assert!(warnings.is_empty());
        assert!(verify_for_test(&source, &destination));
    }

    #[test]
    #[ignore = "requires MINFM_SAMBA_TEST_SOURCE to reference a mounted Samba file"]
    fn samba_source_copy_verifies_contents_without_xattrs() {
        let source = std::env::var_os("MINFM_SAMBA_TEST_SOURCE")
            .map(PathBuf::from)
            .expect("MINFM_SAMBA_TEST_SOURCE is required");
        assert!(source.is_file());
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join(source.file_name().unwrap());
        let operation = spawn(OperationRequest::Copy {
            sources: vec![source.clone()],
            destination: temp.path().to_path_buf(),
            cut: false,
            overwrite: false,
            verify: true,
            current_dir: source.parent().unwrap().to_path_buf(),
            config_dir: temp.path().join("config"),
        });
        let summary = loop {
            if let OperationUpdate::Finished(summary) = operation.receiver.recv().unwrap() {
                break summary;
            }
        };

        assert!(files_match(&source, &destination).unwrap());
        assert_eq!(summary.completed, 1);
        assert!(summary.failed.is_empty());
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("extended attributes are unsupported")));
    }
}
