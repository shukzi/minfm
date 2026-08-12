use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{symlink, MetadataExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    thread,
};

use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use tempfile::Builder as TempBuilder;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const UPDATE_QUEUE_CAPACITY: usize = 128;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const COPY_BUFFER_SIZE: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Tar,
    TarGz,
    Zip,
}

impl ArchiveFormat {
    pub const ALL: [Self; 3] = [Self::TarGz, Self::Zip, Self::Tar];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tar => "TAR",
            Self::TarGz => "TAR.GZ",
            Self::Zip => "ZIP",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Tar => ".tar",
            Self::TarGz => ".tar.gz",
            Self::Zip => ".zip",
        }
    }

    pub fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else if name.ends_with(".zip") {
            Some(Self::Zip)
        } else {
            None
        }
    }

    pub fn append_extension(self, name: &str) -> String {
        let lowercase = name.to_ascii_lowercase();
        let already_matches = match self {
            Self::Tar => lowercase.ends_with(".tar"),
            Self::TarGz => lowercase.ends_with(".tar.gz") || lowercase.ends_with(".tgz"),
            Self::Zip => lowercase.ends_with(".zip"),
        };
        if already_matches {
            name.to_string()
        } else {
            format!("{name}{}", self.extension())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEntryKind {
    File,
    Directory,
    Symlink,
    Hardlink,
    Other,
}

impl ArchiveEntryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Hardlink => "hard link",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: PathBuf,
    pub kind: ArchiveEntryKind,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum ArchiveRequest {
    Create {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        format: ArchiveFormat,
    },
    List {
        archive: PathBuf,
    },
    Extract {
        archive: PathBuf,
        destination: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub enum ArchiveOutcome {
    Created {
        archive: PathBuf,
        entries: usize,
    },
    Listed {
        archive: PathBuf,
        entries: Vec<ArchiveEntry>,
    },
    Extracted {
        archive: PathBuf,
        destination: PathBuf,
        entries: usize,
    },
}

#[derive(Debug)]
pub enum ArchiveUpdate {
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
    Finished(Result<ArchiveOutcome, String>),
}

pub struct RunningArchive {
    pub receiver: Receiver<ArchiveUpdate>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(request: ArchiveRequest) -> RunningArchive {
    let cancel = Arc::new(AtomicBool::new(false));
    spawn_with_cancel(request, cancel)
}

fn spawn_with_cancel(request: ArchiveRequest, cancel: Arc<AtomicBool>) -> RunningArchive {
    let (sender, receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let worker_cancel = Arc::clone(&cancel);
    thread::spawn(move || {
        let result = run(request, &sender, &worker_cancel);
        let _ = sender.send(ArchiveUpdate::Finished(result));
    });
    RunningArchive { receiver, cancel }
}

fn run(
    request: ArchiveRequest,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<ArchiveOutcome, String> {
    match request {
        ArchiveRequest::Create {
            sources,
            destination,
            format,
        } => create_archive(&sources, &destination, format, sender, cancel),
        ArchiveRequest::List { archive } => list_archive(&archive, sender, cancel),
        ArchiveRequest::Extract {
            archive,
            destination,
        } => extract_archive(&archive, &destination, sender, cancel),
    }
}

#[derive(Debug, Clone)]
struct SourceEntry {
    source: PathBuf,
    stored: PathBuf,
    kind: ArchiveEntryKind,
    size: u64,
    mode: u32,
}

fn create_archive(
    sources: &[PathBuf],
    destination: &Path,
    format: ArchiveFormat,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<ArchiveOutcome, String> {
    if sources.is_empty() {
        return Err("No files or directories were selected".into());
    }
    if path_exists(destination)? {
        return Err(format!("Archive already exists: {}", destination.display()));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "Archive destination has no parent directory".to_string())?;
    if !parent.is_dir() {
        return Err(format!(
            "Archive destination directory is unavailable: {}",
            parent.display()
        ));
    }

    let entries = collect_sources(sources, format)?;
    let total_bytes = entries.iter().map(|entry| entry.size).sum();
    ensure_space_available(parent, total_bytes, "create the archive")?;
    let _ = sender.send(ArchiveUpdate::Started {
        label: format!("Creating {} archive", format.label()),
        total_items: entries.len(),
        total_bytes,
    });
    check_cancel(cancel)?;

    let mut temporary = TempBuilder::new()
        .prefix(".minfm-archive-")
        .permissions(fs::Permissions::from_mode(0o666))
        .tempfile_in(parent)
        .map_err(|error| io_message("could not create temporary archive", error))?;

    match format {
        ArchiveFormat::Tar => {
            write_tar(temporary.as_file_mut(), &entries, sender, cancel)?;
        }
        ArchiveFormat::TarGz => {
            let encoder = GzEncoder::new(temporary.as_file_mut(), Compression::default());
            let encoder = write_tar(encoder, &entries, sender, cancel)?;
            encoder
                .finish()
                .map_err(|error| io_message("could not finish gzip compression", error))?;
        }
        ArchiveFormat::Zip => {
            write_zip(temporary.as_file_mut(), &entries, sender, cancel)?;
        }
    }
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| io_message("could not synchronize the archive", error))?;
    check_cancel(cancel)?;
    temporary.persist_noclobber(destination).map_err(|error| {
        format!(
            "Could not install archive {}: {}",
            destination.display(),
            error.error
        )
    })?;

    Ok(ArchiveOutcome::Created {
        archive: destination.to_path_buf(),
        entries: entries.len(),
    })
}

fn collect_sources(sources: &[PathBuf], format: ArchiveFormat) -> Result<Vec<SourceEntry>, String> {
    let mut entries = Vec::new();
    let mut roots = HashSet::new();
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| format!("{} has no archive name", source.display()))?;
        if !roots.insert(name.as_bytes().to_vec()) {
            return Err(format!(
                "Multiple selected items would use the same archive path: {}",
                name.to_string_lossy()
            ));
        }
        collect_source(source, Path::new(name), format, &mut entries)?;
        if entries.len() > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "Selection contains more than {MAX_ARCHIVE_ENTRIES} archive entries"
            ));
        }
    }
    Ok(entries)
}

fn collect_source(
    source: &Path,
    stored: &Path,
    format: ArchiveFormat,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), String> {
    validate_stored_path(stored)?;
    if format == ArchiveFormat::Zip && stored.to_str().is_none() {
        return Err(format!(
            "ZIP archives require UTF-8 filenames: {}",
            source.display()
        ));
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| io_message(&format!("could not inspect {}", source.display()), error))?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        ArchiveEntryKind::Directory
    } else if file_type.is_file() {
        ArchiveEntryKind::File
    } else if file_type.is_symlink() {
        ArchiveEntryKind::Symlink
    } else {
        return Err(format!(
            "Unsupported filesystem entry: {}",
            source.display()
        ));
    };
    entries.push(SourceEntry {
        source: source.to_path_buf(),
        stored: stored.to_path_buf(),
        kind,
        size: if kind == ArchiveEntryKind::File {
            metadata.len()
        } else {
            0
        },
        mode: metadata.mode(),
    });
    if kind == ArchiveEntryKind::Directory {
        let mut children = fs::read_dir(source)
            .map_err(|error| io_message(&format!("could not read {}", source.display()), error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_message(&format!("could not read {}", source.display()), error))?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_bytes()
                .cmp(right.file_name().as_bytes())
        });
        for child in children {
            collect_source(
                &child.path(),
                &stored.join(child.file_name()),
                format,
                entries,
            )?;
        }
    }
    Ok(())
}

fn write_tar<W: Write>(
    writer: W,
    entries: &[SourceEntry],
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<W, String> {
    let mut builder = tar::Builder::new(writer);
    let mut completed_bytes: u64 = 0;
    for (index, entry) in entries.iter().enumerate() {
        check_cancel(cancel)?;
        let metadata = fs::symlink_metadata(&entry.source).map_err(|error| {
            io_message(
                &format!("could not inspect {}", entry.source.display()),
                error,
            )
        })?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&metadata);
        header
            .set_path(&entry.stored)
            .map_err(|error| io_message("could not store an archive path", error))?;
        match entry.kind {
            ArchiveEntryKind::File => {
                let file = File::open(&entry.source).map_err(|error| {
                    io_message(&format!("could not open {}", entry.source.display()), error)
                })?;
                header.set_size(entry.size);
                header.set_cksum();
                let mut reader = CancelReader::new(file.take(entry.size), cancel);
                builder.append(&header, &mut reader).map_err(|error| {
                    archive_io_message("could not write TAR entry", error, cancel)
                })?;
                if reader.inner.limit() != 0 {
                    return Err(format!(
                        "Source changed while creating the archive: {}",
                        entry.source.display()
                    ));
                }
                completed_bytes = completed_bytes.saturating_add(entry.size);
            }
            ArchiveEntryKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                builder.append(&header, io::empty()).map_err(|error| {
                    archive_io_message("could not write TAR directory", error, cancel)
                })?;
            }
            ArchiveEntryKind::Symlink => {
                let target = fs::read_link(&entry.source).map_err(|error| {
                    io_message(
                        &format!("could not read link {}", entry.source.display()),
                        error,
                    )
                })?;
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header
                    .set_link_name(&target)
                    .map_err(|error| io_message("could not store symlink target", error))?;
                header.set_cksum();
                builder.append(&header, io::empty()).map_err(|error| {
                    archive_io_message("could not write TAR symlink", error, cancel)
                })?;
            }
            ArchiveEntryKind::Hardlink | ArchiveEntryKind::Other => {
                return Err("Unsupported TAR source entry".into())
            }
        }
        send_progress(sender, &entry.stored, index + 1, completed_bytes);
    }
    builder
        .finish()
        .map_err(|error| archive_io_message("could not finish TAR archive", error, cancel))?;
    builder
        .into_inner()
        .map_err(|error| archive_io_message("could not close TAR archive", error, cancel))
}

fn write_zip(
    writer: &mut File,
    entries: &[SourceEntry],
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut zip = ZipWriter::new(writer);
    let mut completed_bytes: u64 = 0;
    for (index, entry) in entries.iter().enumerate() {
        check_cancel(cancel)?;
        let name = zip_name(&entry.stored)?;
        let permissions = entry.mode & 0o7777;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(permissions);
        match entry.kind {
            ArchiveEntryKind::Directory => {
                zip.add_directory(format!("{}/", name.trim_end_matches('/')), options)
                    .map_err(|error| format!("Could not write ZIP directory: {error}"))?;
            }
            ArchiveEntryKind::File => {
                zip.start_file(name, options)
                    .map_err(|error| format!("Could not write ZIP file header: {error}"))?;
                let file = File::open(&entry.source).map_err(|error| {
                    io_message(&format!("could not open {}", entry.source.display()), error)
                })?;
                let mut reader = CancelReader::new(file.take(entry.size), cancel);
                let copied = io::copy(&mut reader, &mut zip).map_err(|error| {
                    archive_io_message("could not write ZIP file", error, cancel)
                })?;
                if copied != entry.size {
                    return Err(format!(
                        "Source changed while creating the archive: {}",
                        entry.source.display()
                    ));
                }
                completed_bytes = completed_bytes.saturating_add(entry.size);
            }
            ArchiveEntryKind::Symlink => {
                let target = fs::read_link(&entry.source).map_err(|error| {
                    io_message(
                        &format!("could not read link {}", entry.source.display()),
                        error,
                    )
                })?;
                let target = target.to_str().ok_or_else(|| {
                    format!(
                        "ZIP archives require UTF-8 symlink targets: {}",
                        entry.source.display()
                    )
                })?;
                zip.add_symlink(name, target, options)
                    .map_err(|error| format!("Could not write ZIP symlink: {error}"))?;
            }
            ArchiveEntryKind::Hardlink | ArchiveEntryKind::Other => {
                return Err("Unsupported ZIP source entry".into())
            }
        }
        send_progress(sender, &entry.stored, index + 1, completed_bytes);
    }
    zip.finish()
        .map_err(|error| format!("Could not finish ZIP archive: {error}"))?;
    Ok(())
}

fn list_archive(
    archive: &Path,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<ArchiveOutcome, String> {
    let format = ArchiveFormat::detect(archive)
        .ok_or_else(|| format!("Unsupported archive format: {}", archive.to_string_lossy()))?;
    let _ = sender.send(ArchiveUpdate::Started {
        label: format!("Inspecting {} archive", format.label()),
        total_items: 0,
        total_bytes: 0,
    });
    let entries = read_archive_entries(archive, format, sender, cancel)?;
    Ok(ArchiveOutcome::Listed {
        archive: archive.to_path_buf(),
        entries,
    })
}

fn read_archive_entries(
    archive: &Path,
    format: ArchiveFormat,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<Vec<ArchiveEntry>, String> {
    match format {
        ArchiveFormat::Tar => {
            let file = File::open(archive).map_err(|error| {
                io_message(&format!("could not open {}", archive.display()), error)
            })?;
            read_tar_entries(CancelReader::new(file, cancel), sender, cancel)
        }
        ArchiveFormat::TarGz => {
            let file = File::open(archive).map_err(|error| {
                io_message(&format!("could not open {}", archive.display()), error)
            })?;
            let decoder = MultiGzDecoder::new(CancelReader::new(file, cancel));
            read_tar_entries(decoder, sender, cancel)
        }
        ArchiveFormat::Zip => read_zip_entries(archive, sender, cancel),
    }
}

fn read_tar_entries<R: Read>(
    reader: R,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<Vec<ArchiveEntry>, String> {
    let mut archive = tar::Archive::new(reader);
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    let entries = archive
        .entries()
        .map_err(|error| archive_io_message("could not read TAR archive", error, cancel))?;
    for entry in entries {
        check_cancel(cancel)?;
        let entry =
            entry.map_err(|error| archive_io_message("could not read TAR entry", error, cancel))?;
        let path = entry
            .path()
            .map_err(|error| archive_io_message("could not read TAR path", error, cancel))?
            .into_owned();
        let path = normalize_stored_path(&path)?;
        if !seen.insert(path.clone()) {
            return Err(format!(
                "Archive contains a duplicate path: {}",
                path.display()
            ));
        }
        let entry_type = entry.header().entry_type();
        let kind = if entry_type.is_file() {
            ArchiveEntryKind::File
        } else if entry_type.is_dir() {
            ArchiveEntryKind::Directory
        } else if entry_type.is_symlink() {
            validate_link_target(
                &path,
                entry
                    .link_name()
                    .map_err(|error| {
                        archive_io_message("could not read TAR symlink target", error, cancel)
                    })?
                    .map(|target| target.into_owned()),
            )?;
            ArchiveEntryKind::Symlink
        } else if entry_type.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|error| {
                    archive_io_message("could not read TAR hard-link target", error, cancel)
                })?
                .map(|target| target.into_owned())
                .ok_or_else(|| format!("Archive hard link has no target: {}", path.display()))?;
            normalize_stored_path(&target)?;
            ArchiveEntryKind::Hardlink
        } else {
            ArchiveEntryKind::Other
        };
        let size = entry.header().size().unwrap_or(0);
        results.push(ArchiveEntry {
            path: path.clone(),
            kind,
            size,
        });
        if results.len() > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "Archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            ));
        }
        send_progress(sender, &path, results.len(), 0);
    }
    Ok(results)
}

fn read_zip_entries(
    archive: &Path,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<Vec<ArchiveEntry>, String> {
    let file = File::open(archive)
        .map_err(|error| io_message(&format!("could not open {}", archive.display()), error))?;
    let mut zip =
        ZipArchive::new(file).map_err(|error| format!("Could not read ZIP archive: {error}"))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "Archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
        ));
    }
    if zip
        .has_overlapping_files()
        .map_err(|error| format!("Could not validate ZIP archive: {error}"))?
    {
        return Err("ZIP archive contains overlapping file data".into());
    }
    let mut results = Vec::with_capacity(zip.len());
    let mut seen = HashSet::new();
    for index in 0..zip.len() {
        check_cancel(cancel)?;
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("Could not read ZIP entry: {error}"))?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| format!("ZIP archive contains an unsafe path: {}", entry.name()))?;
        let path = normalize_stored_path(&path)?;
        if !seen.insert(path.clone()) {
            return Err(format!(
                "Archive contains a duplicate path: {}",
                path.display()
            ));
        }
        let mode = entry.unix_mode().unwrap_or(0);
        let kind = if entry.is_dir() {
            ArchiveEntryKind::Directory
        } else if mode & 0o170000 == 0o120000 {
            let target = read_zip_link_target(&mut entry)?;
            validate_link_target(&path, Some(target))?;
            ArchiveEntryKind::Symlink
        } else {
            ArchiveEntryKind::File
        };
        let size = entry.size();
        results.push(ArchiveEntry {
            path: path.clone(),
            kind,
            size,
        });
        send_progress(sender, &path, results.len(), 0);
    }
    Ok(results)
}

fn extract_archive(
    archive_path: &Path,
    destination: &Path,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<ArchiveOutcome, String> {
    if !destination.is_dir() {
        return Err(format!(
            "Extraction destination is not a directory: {}",
            destination.display()
        ));
    }
    let format = ArchiveFormat::detect(archive_path).ok_or_else(|| {
        format!(
            "Unsupported archive format: {}",
            archive_path.to_string_lossy()
        )
    })?;
    let _ = sender.send(ArchiveUpdate::Started {
        label: format!("Inspecting {} archive", format.label()),
        total_items: 0,
        total_bytes: 0,
    });
    let entries = read_archive_entries(archive_path, format, sender, cancel)?;
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.kind == ArchiveEntryKind::Other)
    {
        return Err(format!(
            "Archive contains an unsupported special entry: {}",
            entry.path.display()
        ));
    }
    let total_bytes = entries.iter().map(|entry| entry.size).sum();
    ensure_space_available(destination, total_bytes, "extract the archive")?;
    let _ = sender.send(ArchiveUpdate::Started {
        label: format!("Extracting {} archive", format.label()),
        total_items: entries.len(),
        total_bytes,
    });
    check_cancel(cancel)?;

    let staging = TempBuilder::new()
        .prefix(".minfm-extract-")
        .tempdir_in(destination)
        .map_err(|error| io_message("could not create extraction staging directory", error))?;
    match format {
        ArchiveFormat::Tar => {
            let file = File::open(archive_path).map_err(|error| {
                io_message(&format!("could not open {}", archive_path.display()), error)
            })?;
            extract_tar(
                CancelReader::new(file, cancel),
                staging.path(),
                sender,
                cancel,
            )?;
        }
        ArchiveFormat::TarGz => {
            let file = File::open(archive_path).map_err(|error| {
                io_message(&format!("could not open {}", archive_path.display()), error)
            })?;
            extract_tar(
                MultiGzDecoder::new(CancelReader::new(file, cancel)),
                staging.path(),
                sender,
                cancel,
            )?;
        }
        ArchiveFormat::Zip => {
            extract_zip(archive_path, staging.path(), sender, cancel)?;
        }
    }
    check_cancel(cancel)?;
    commit_staging(staging.path(), destination)?;
    staging
        .close()
        .map_err(|error| io_message("could not clean extraction staging directory", error))?;
    Ok(ArchiveOutcome::Extracted {
        archive: archive_path.to_path_buf(),
        destination: destination.to_path_buf(),
        entries: entries.len(),
    })
}

fn extract_tar<R: Read>(
    reader: R,
    staging: &Path,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    let entries = archive
        .entries()
        .map_err(|error| archive_io_message("could not read TAR archive", error, cancel))?;
    let mut completed = 0;
    let mut completed_bytes: u64 = 0;
    for entry in entries {
        check_cancel(cancel)?;
        let mut entry =
            entry.map_err(|error| archive_io_message("could not read TAR entry", error, cancel))?;
        let path = entry
            .path()
            .map_err(|error| archive_io_message("could not read TAR path", error, cancel))?
            .into_owned();
        let path = normalize_stored_path(&path)?;
        ensure_parents_are_directories(staging, &path)?;
        let size = entry.header().size().unwrap_or(0);
        let extracted = entry
            .unpack_in(staging)
            .map_err(|error| archive_io_message("could not extract TAR entry", error, cancel))?;
        if !extracted {
            return Err(format!(
                "TAR archive contains an unsafe path: {}",
                path.display()
            ));
        }
        completed += 1;
        completed_bytes = completed_bytes.saturating_add(size);
        send_progress(sender, &path, completed, completed_bytes);
    }
    Ok(())
}

fn extract_zip(
    archive_path: &Path,
    staging: &Path,
    sender: &SyncSender<ArchiveUpdate>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|error| {
        io_message(&format!("could not open {}", archive_path.display()), error)
    })?;
    let mut zip =
        ZipArchive::new(file).map_err(|error| format!("Could not read ZIP archive: {error}"))?;
    let mut completed_bytes: u64 = 0;
    let mut directory_modes = Vec::new();
    for index in 0..zip.len() {
        check_cancel(cancel)?;
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("Could not read ZIP entry: {error}"))?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| format!("ZIP archive contains an unsafe path: {}", entry.name()))?;
        let path = normalize_stored_path(&path)?;
        ensure_parents_are_directories(staging, &path)?;
        let output = staging.join(&path);
        let mode = entry.unix_mode().unwrap_or(0);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|error| {
                io_message(&format!("could not create {}", output.display()), error)
            })?;
            if mode != 0 {
                directory_modes.push((output.clone(), mode & 0o7777));
            }
        } else if mode & 0o170000 == 0o120000 {
            let target = read_zip_link_target(&mut entry)?;
            validate_link_target(&path, Some(target.clone()))?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    io_message(&format!("could not create {}", parent.display()), error)
                })?;
            }
            symlink(target, &output).map_err(|error| {
                io_message(&format!("could not create {}", output.display()), error)
            })?;
        } else {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    io_message(&format!("could not create {}", parent.display()), error)
                })?;
            }
            ensure_parents_are_directories(staging, &path)?;
            let mut output_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|error| {
                    io_message(&format!("could not create {}", output.display()), error)
                })?;
            copy_cancellable(&mut entry, &mut output_file, cancel)?;
            if mode != 0 {
                fs::set_permissions(&output, fs::Permissions::from_mode(mode & 0o7777)).map_err(
                    |error| {
                        io_message(
                            &format!("could not set permissions on {}", output.display()),
                            error,
                        )
                    },
                )?;
            }
        }
        completed_bytes = completed_bytes.saturating_add(entry.size());
        send_progress(sender, &path, index + 1, completed_bytes);
    }
    directory_modes.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, mode) in directory_modes {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).map_err(|error| {
            io_message(
                &format!("could not set permissions on {}", path.display()),
                error,
            )
        })?;
    }
    Ok(())
}

fn commit_staging(staging: &Path, destination: &Path) -> Result<(), String> {
    let mut entries = fs::read_dir(staging)
        .map_err(|error| io_message("could not inspect extraction output", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_message("could not inspect extraction output", error))?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_bytes()
            .cmp(right.file_name().as_bytes())
    });
    for entry in &entries {
        let target = destination.join(entry.file_name());
        if path_exists(&target)? {
            return Err(format!(
                "Extraction would overwrite an existing item: {}",
                target.display()
            ));
        }
    }
    let mut moved = Vec::new();
    for entry in entries {
        let source = entry.path();
        let target = destination.join(entry.file_name());
        if let Err(error) = rename_noreplace(&source, &target) {
            for (installed, original) in moved.into_iter().rev() {
                let _ = fs::rename(installed, original);
            }
            return Err(format!(
                "Could not install extracted item {} without overwriting it: {error}",
                target.display()
            ));
        }
        moved.push((target, source));
    }
    Ok(())
}

fn rename_noreplace(source: &Path, destination: &Path) -> rustix::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
}

fn validate_stored_path(path: &Path) -> Result<(), String> {
    normalize_stored_path(path).map(|_| ())
}

fn normalize_stored_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("Archive contains an empty path".into());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => normalized.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Archive contains an unsafe path: {}",
                    path.display()
                ))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("Archive contains an empty path".into());
    }
    Ok(normalized)
}

fn validate_link_target(path: &Path, target: Option<PathBuf>) -> Result<(), String> {
    let target =
        target.ok_or_else(|| format!("Archive link has no target: {}", path.to_string_lossy()))?;
    if target.is_absolute() {
        return Err(format!(
            "Archive link escapes the extraction directory: {} -> {}",
            path.display(),
            target.display()
        ));
    }
    let base = path.parent().unwrap_or_else(|| Path::new(""));
    let mut depth: usize = 0;
    for component in base.join(&target).components() {
        match component {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(format!(
                        "Archive link escapes the extraction directory: {} -> {}",
                        path.display(),
                        target.display()
                    ));
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Archive link escapes the extraction directory: {} -> {}",
                    path.display(),
                    target.display()
                ))
            }
        }
    }
    Ok(())
}

fn ensure_parents_are_directories(root: &Path, relative: &Path) -> Result<(), String> {
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Archive entry would traverse a symbolic link: {}",
                    relative.display()
                ))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Archive entry has a non-directory parent: {}",
                    relative.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(io_message(
                    &format!("could not inspect {}", current.display()),
                    error,
                ))
            }
        }
    }
    Ok(())
}

fn read_zip_link_target<R: Read>(reader: &mut R) -> Result<PathBuf, String> {
    let mut bytes = Vec::new();
    reader
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|error| io_message("could not read ZIP symlink target", error))?;
    if bytes.len() > 4096 {
        return Err("ZIP symlink target is unreasonably long".into());
    }
    if bytes.contains(&0) {
        return Err("ZIP symlink target contains a null byte".into());
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
}

fn zip_name(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| format!("ZIP archives require UTF-8 paths: {}", path.display()))
}

fn path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_message(
            &format!("could not inspect {}", path.display()),
            error,
        )),
    }
}

fn ensure_space_available(path: &Path, payload_bytes: u64, operation: &str) -> Result<(), String> {
    let Ok(stats) = rustix::fs::statvfs(path) else {
        return Ok(());
    };
    let available = stats.f_bavail.saturating_mul(stats.f_frsize);
    let overhead = payload_bytes / 20 + 16 * 1024 * 1024;
    let required = payload_bytes.saturating_add(overhead);
    if available < required {
        return Err(format!(
            "Not enough free space to {operation}: need approximately {} bytes, have {} bytes",
            required, available
        ));
    }
    Ok(())
}

fn copy_cancellable(
    reader: &mut impl Read,
    writer: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    let mut buffer = vec![0; COPY_BUFFER_SIZE];
    let mut written: u64 = 0;
    loop {
        check_cancel(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| archive_io_message("could not read archive data", error, cancel))?;
        if count == 0 {
            break;
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| archive_io_message("could not write archive data", error, cancel))?;
        written = written.saturating_add(count as u64);
    }
    Ok(written)
}

struct CancelReader<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}

impl<'a, R> CancelReader<'a, R> {
    fn new(inner: R, cancel: &'a AtomicBool) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for CancelReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ));
        }
        let count = buffer.len().min(COPY_BUFFER_SIZE);
        self.inner.read(&mut buffer[..count])
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Archive operation cancelled".into())
    } else {
        Ok(())
    }
}

fn send_progress(
    sender: &SyncSender<ArchiveUpdate>,
    current: &Path,
    completed_items: usize,
    completed_bytes: u64,
) {
    match sender.try_send(ArchiveUpdate::Progress {
        current: current.to_path_buf(),
        completed_items,
        completed_bytes,
    }) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn io_message(context: &str, error: io::Error) -> String {
    format!("{context}: {error}")
}

fn archive_io_message(context: &str, error: io::Error, cancel: &AtomicBool) -> String {
    if cancel.load(Ordering::Relaxed) || error.kind() == io::ErrorKind::Interrupted {
        "Archive operation cancelled".into()
    } else {
        io_message(context, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::sync::mpsc;

    fn channel() -> (SyncSender<ArchiveUpdate>, Receiver<ArchiveUpdate>) {
        mpsc::sync_channel(UPDATE_QUEUE_CAPACITY)
    }

    fn sample_tree(root: &Path) -> Vec<PathBuf> {
        let folder = root.join("sample");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("alpha.txt"), b"alpha contents").unwrap();
        fs::create_dir(folder.join("nested")).unwrap();
        fs::write(folder.join("nested/beta.txt"), b"beta contents").unwrap();
        symlink("alpha.txt", folder.join("alpha-link")).unwrap();
        vec![folder]
    }

    #[test]
    fn format_detection_and_extensions_are_stable() {
        assert_eq!(
            ArchiveFormat::detect(Path::new("backup.TAR.GZ")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::detect(Path::new("backup.tgz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::detect(Path::new("backup.tar")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(
            ArchiveFormat::detect(Path::new("backup.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(ArchiveFormat::detect(Path::new("backup.gz")), None);
        assert_eq!(ArchiveFormat::Zip.append_extension("backup"), "backup.zip");
        assert_eq!(
            ArchiveFormat::TarGz.append_extension("backup.tgz"),
            "backup.tgz"
        );
    }

    #[test]
    fn every_supported_format_round_trips_files_directories_and_safe_links() {
        for format in ArchiveFormat::ALL {
            let temp = tempfile::tempdir().unwrap();
            let sources = sample_tree(temp.path());
            let archive_path = temp.path().join(format!("bundle{}", format.extension()));
            let (sender, _receiver) = channel();
            let cancel = AtomicBool::new(false);
            let outcome =
                create_archive(&sources, &archive_path, format, &sender, &cancel).unwrap();
            assert!(matches!(outcome, ArchiveOutcome::Created { .. }));

            let listed = read_archive_entries(&archive_path, format, &sender, &cancel).unwrap();
            assert!(listed.iter().any(|entry| {
                entry.path == Path::new("sample/nested/beta.txt")
                    && entry.kind == ArchiveEntryKind::File
            }));
            assert!(listed.iter().any(|entry| {
                entry.path == Path::new("sample/alpha-link")
                    && entry.kind == ArchiveEntryKind::Symlink
            }));

            let destination = temp.path().join("output");
            fs::create_dir(&destination).unwrap();
            let outcome = extract_archive(&archive_path, &destination, &sender, &cancel).unwrap();
            assert!(matches!(outcome, ArchiveOutcome::Extracted { .. }));
            assert_eq!(
                fs::read(destination.join("sample/alpha.txt")).unwrap(),
                b"alpha contents"
            );
            assert_eq!(
                fs::read(destination.join("sample/nested/beta.txt")).unwrap(),
                b"beta contents"
            );
            assert_eq!(
                fs::read_link(destination.join("sample/alpha-link")).unwrap(),
                Path::new("alpha.txt")
            );
        }
    }

    #[test]
    fn creation_never_overwrites_an_existing_archive() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        fs::write(&source, b"source").unwrap();
        let destination = temp.path().join("bundle.zip");
        fs::write(&destination, b"keep me").unwrap();
        let (sender, _receiver) = channel();
        let error = create_archive(
            &[source],
            &destination,
            ArchiveFormat::Zip,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(fs::read(destination).unwrap(), b"keep me");
    }

    #[test]
    fn extraction_rejects_existing_destinations_without_partial_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        fs::write(&source, b"new data").unwrap();
        let archive_path = temp.path().join("bundle.tar.gz");
        let (sender, _receiver) = channel();
        let cancel = AtomicBool::new(false);
        create_archive(
            &[source],
            &archive_path,
            ArchiveFormat::TarGz,
            &sender,
            &cancel,
        )
        .unwrap();
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("source.txt"), b"existing data").unwrap();

        let error = extract_archive(&archive_path, &destination, &sender, &cancel).unwrap_err();
        assert!(error.contains("would overwrite"));
        assert_eq!(
            fs::read(destination.join("source.txt")).unwrap(),
            b"existing data"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    }

    #[test]
    fn final_install_never_replaces_a_racing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let staged = temp.path().join("staged.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&staged, b"archive data").unwrap();
        fs::write(&destination, b"racing data").unwrap();
        assert!(rename_noreplace(&staged, &destination).is_err());
        assert_eq!(fs::read(staged).unwrap(), b"archive data");
        assert_eq!(fs::read(destination).unwrap(), b"racing data");
    }

    #[test]
    fn creation_fails_cleanly_if_a_source_shrinks_after_collection() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("changing.txt");
        fs::write(&source, b"short").unwrap();
        let entry = SourceEntry {
            source: source.clone(),
            stored: PathBuf::from("changing.txt"),
            kind: ArchiveEntryKind::File,
            size: 10,
            mode: fs::metadata(&source).unwrap().mode(),
        };
        let (sender, _receiver) = channel();
        let cancel = AtomicBool::new(false);
        let tar_error =
            write_tar(Vec::new(), std::slice::from_ref(&entry), &sender, &cancel).unwrap_err();
        assert!(tar_error.contains("Source changed"));

        let zip_path = temp.path().join("changing.zip");
        let mut zip_file = File::create(zip_path).unwrap();
        let zip_error = write_zip(&mut zip_file, &[entry], &sender, &cancel).unwrap_err();
        assert!(zip_error.contains("Source changed"));
    }

    #[test]
    fn unsafe_zip_paths_are_rejected_before_extraction() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("unsafe.zip");
        let file = File::create(&archive_path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("../escape.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"escape").unwrap();
        writer.finish().unwrap();
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();
        let (sender, _receiver) = channel();
        let error = extract_archive(
            &archive_path,
            &destination,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("unsafe path"));
        assert!(!temp.path().join("escape.txt").exists());
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    fn unsafe_zip_symlink_targets_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("unsafe-link.zip");
        let file = File::create(&archive_path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .add_symlink("folder/link", "../../escape", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        let (sender, _receiver) = channel();
        let error = read_archive_entries(
            &archive_path,
            ArchiveFormat::Zip,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("escapes the extraction directory"));
    }

    #[test]
    fn zip_writer_rejects_duplicate_archive_paths() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("duplicate.zip");
        let file = File::create(&archive_path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("same.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"first").unwrap();
        let error = writer
            .start_file("same.txt", SimpleFileOptions::default())
            .unwrap_err();
        assert!(error.to_string().contains("Duplicate filename"));
    }

    #[test]
    fn malformed_archives_return_errors_without_output() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("broken.zip");
        fs::write(&archive_path, b"not a zip archive").unwrap();
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();
        let (sender, _receiver) = channel();
        assert!(extract_archive(
            &archive_path,
            &destination,
            &sender,
            &AtomicBool::new(false),
        )
        .is_err());
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    fn cancellation_removes_temporary_archive_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        fs::write(&source, b"source").unwrap();
        let destination = temp.path().join("cancelled.tar");
        let (sender, _receiver) = channel();
        let cancel = AtomicBool::new(true);
        let error = create_archive(
            &[source],
            &destination,
            ArchiveFormat::Tar,
            &sender,
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(!destination.exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".minfm-archive-")));
    }

    #[test]
    fn path_validation_rejects_absolute_and_parent_paths() {
        assert!(validate_stored_path(Path::new("safe/path")).is_ok());
        assert!(validate_stored_path(Path::new("../escape")).is_err());
        assert!(validate_stored_path(Path::new("/absolute")).is_err());
        assert!(
            validate_link_target(Path::new("folder/link"), Some(PathBuf::from("../target")))
                .is_ok()
        );
        assert!(validate_link_target(
            Path::new("folder/link"),
            Some(PathBuf::from("../../escape"))
        )
        .is_err());
    }

    #[test]
    fn unicode_and_space_filenames_round_trip_in_every_format() {
        for format in ArchiveFormat::ALL {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("sp ace-λ.txt");
            fs::write(&source, "unicode payload").unwrap();
            let archive_path = temp.path().join(format!("unicode{}", format.extension()));
            let (sender, _receiver) = channel();
            let cancel = AtomicBool::new(false);
            create_archive(&[source], &archive_path, format, &sender, &cancel).unwrap();
            let output = temp.path().join("output");
            fs::create_dir(&output).unwrap();
            extract_archive(&archive_path, &output, &sender, &cancel).unwrap();
            assert_eq!(
                fs::read_to_string(output.join("sp ace-λ.txt")).unwrap(),
                "unicode payload"
            );
        }
    }

    #[test]
    fn tar_formats_round_trip_non_utf8_filenames_and_zip_rejects_them() {
        for format in [ArchiveFormat::Tar, ArchiveFormat::TarGz] {
            let temp = tempfile::tempdir().unwrap();
            let name = OsStr::from_bytes(b"non-utf8-\xff.txt");
            let source = temp.path().join(name);
            fs::write(&source, b"non-utf8 payload").unwrap();
            let archive_path = temp.path().join(format!("bytes{}", format.extension()));
            let (sender, _receiver) = channel();
            let cancel = AtomicBool::new(false);
            create_archive(&[source], &archive_path, format, &sender, &cancel).unwrap();
            let output = temp.path().join("output");
            fs::create_dir(&output).unwrap();
            extract_archive(&archive_path, &output, &sender, &cancel).unwrap();
            assert_eq!(fs::read(output.join(name)).unwrap(), b"non-utf8 payload");
        }

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join(OsStr::from_bytes(b"non-utf8-\xff.txt"));
        fs::write(&source, b"payload").unwrap();
        let (sender, _receiver) = channel();
        let error = create_archive(
            &[source],
            &temp.path().join("bytes.zip"),
            ArchiveFormat::Zip,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("require UTF-8 filenames"));
    }

    #[test]
    fn empty_archives_list_and_extract_cleanly() {
        for format in ArchiveFormat::ALL {
            let temp = tempfile::tempdir().unwrap();
            let archive_path = temp.path().join(format!("empty{}", format.extension()));
            match format {
                ArchiveFormat::Tar => {
                    tar::Builder::new(File::create(&archive_path).unwrap())
                        .finish()
                        .unwrap();
                }
                ArchiveFormat::TarGz => {
                    let encoder = GzEncoder::new(
                        File::create(&archive_path).unwrap(),
                        Compression::default(),
                    );
                    let mut builder = tar::Builder::new(encoder);
                    builder.finish().unwrap();
                    builder.into_inner().unwrap().finish().unwrap();
                }
                ArchiveFormat::Zip => {
                    ZipWriter::new(File::create(&archive_path).unwrap())
                        .finish()
                        .unwrap();
                }
            }
            let output = temp.path().join("output");
            fs::create_dir(&output).unwrap();
            let (sender, _receiver) = channel();
            let cancel = AtomicBool::new(false);
            assert!(
                read_archive_entries(&archive_path, format, &sender, &cancel)
                    .unwrap()
                    .is_empty()
            );
            extract_archive(&archive_path, &output, &sender, &cancel).unwrap();
            assert_eq!(fs::read_dir(output).unwrap().count(), 0);
        }
    }

    #[test]
    fn tar_hard_links_are_confined_and_extracted() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("hard-link.tar");
        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut file_header = tar::Header::new_gnu();
        file_header.set_path("original.txt").unwrap();
        file_header.set_size(7);
        file_header.set_mode(0o640);
        file_header.set_cksum();
        builder.append(&file_header, &b"payload"[..]).unwrap();
        let mut link_header = tar::Header::new_gnu();
        link_header.set_path("linked.txt").unwrap();
        link_header.set_entry_type(tar::EntryType::Link);
        link_header.set_link_name("original.txt").unwrap();
        link_header.set_size(0);
        link_header.set_mode(0o640);
        link_header.set_cksum();
        builder.append(&link_header, io::empty()).unwrap();
        builder.finish().unwrap();

        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        let (sender, _receiver) = channel();
        extract_archive(&archive_path, &output, &sender, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(output.join("linked.txt")).unwrap(), b"payload");
        assert_eq!(
            fs::metadata(output.join("original.txt")).unwrap().ino(),
            fs::metadata(output.join("linked.txt")).unwrap().ino()
        );
    }

    #[test]
    fn multiple_selected_roots_are_created_and_extracted_together() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let archive_path = temp.path().join("selection.zip");
        let (sender, _receiver) = channel();
        let cancel = AtomicBool::new(false);
        create_archive(
            &[first, second],
            &archive_path,
            ArchiveFormat::Zip,
            &sender,
            &cancel,
        )
        .unwrap();
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        extract_archive(&archive_path, &output, &sender, &cancel).unwrap();
        assert_eq!(fs::read(output.join("first.txt")).unwrap(), b"first");
        assert_eq!(fs::read(output.join("second.txt")).unwrap(), b"second");
    }

    #[test]
    fn normalized_duplicate_tar_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("duplicate.tar");
        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        for path in ["./same.txt", "same.txt"] {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &b"x"[..]).unwrap();
        }
        builder.finish().unwrap();
        let (sender, _receiver) = channel();
        let error = read_archive_entries(
            &archive_path,
            ArchiveFormat::Tar,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("duplicate path"));
    }

    #[test]
    fn unsafe_tar_link_targets_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("unsafe-link.tar");
        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_path("folder/link").unwrap();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("../../escape").unwrap();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append(&header, io::empty()).unwrap();
        builder.finish().unwrap();
        let (sender, _receiver) = channel();
        let error = read_archive_entries(
            &archive_path,
            ArchiveFormat::Tar,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("escapes the extraction directory"));
    }

    #[test]
    fn special_tar_entries_are_rejected_before_extraction() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("special.tar");
        let file = File::create(&archive_path).unwrap();
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_path("pipe").unwrap();
        header.set_entry_type(tar::EntryType::Fifo);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append(&header, io::empty()).unwrap();
        builder.finish().unwrap();
        let destination = temp.path().join("output");
        fs::create_dir(&destination).unwrap();
        let (sender, _receiver) = channel();
        let error = extract_archive(
            &archive_path,
            &destination,
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("unsupported special entry"));
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    fn file_and_directory_permissions_are_preserved() {
        for format in ArchiveFormat::ALL {
            let temp = tempfile::tempdir().unwrap();
            let folder = temp.path().join("private");
            fs::create_dir(&folder).unwrap();
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o750)).unwrap();
            let source = folder.join("data.txt");
            fs::write(&source, b"permissions").unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
            let archive_path = temp.path().join(format!("modes{}", format.extension()));
            let (sender, _receiver) = channel();
            let cancel = AtomicBool::new(false);
            create_archive(&[folder], &archive_path, format, &sender, &cancel).unwrap();
            let output = temp.path().join("output");
            fs::create_dir(&output).unwrap();
            extract_archive(&archive_path, &output, &sender, &cancel).unwrap();
            assert_eq!(
                fs::metadata(output.join("private")).unwrap().mode() & 0o777,
                0o750
            );
            assert_eq!(
                fs::metadata(output.join("private/data.txt"))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn asynchronous_precancellation_returns_cleanly_without_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("large.bin");
        let file = File::create(&source).unwrap();
        file.set_len(16 * 1024 * 1024).unwrap();
        let destination = temp.path().join("cancelled.tar.gz");
        let cancel = Arc::new(AtomicBool::new(true));
        let running = spawn_with_cancel(
            ArchiveRequest::Create {
                sources: vec![source],
                destination: destination.clone(),
                format: ArchiveFormat::TarGz,
            },
            cancel,
        );
        let started = running
            .receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(matches!(started, ArchiveUpdate::Started { .. }));
        let result = loop {
            match running
                .receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
            {
                ArchiveUpdate::Finished(result) => break result,
                ArchiveUpdate::Started { .. } | ArchiveUpdate::Progress { .. } => {}
            }
        };
        assert_eq!(result.unwrap_err(), "Archive operation cancelled");
        assert!(!destination.exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".minfm-archive-")
        }));
    }

    #[test]
    fn cancellable_reader_limits_the_time_spent_in_each_inner_read() {
        struct ProbeReader {
            largest_request: usize,
        }

        impl Read for ProbeReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.largest_request = self.largest_request.max(buffer.len());
                Ok(0)
            }
        }

        let cancel = AtomicBool::new(false);
        let mut reader = CancelReader::new(ProbeReader { largest_request: 0 }, &cancel);
        let mut buffer = vec![0; COPY_BUFFER_SIZE * 8];
        assert_eq!(reader.read(&mut buffer).unwrap(), 0);
        assert_eq!(reader.inner.largest_request, COPY_BUFFER_SIZE);
    }
}
