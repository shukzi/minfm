use std::{
    cmp::Ordering,
    ffi::OsStr,
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Local};

use crate::{config::SortSetting, error::Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    BlockDevice,
    Other,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub modified: Option<SystemTime>,
    pub selected: bool,
}

impl FileEntry {
    pub fn from_path(path: PathBuf) -> std::io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_block_device() {
            EntryKind::BlockDevice
        } else {
            EntryKind::Other
        };
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("/"))
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            path,
            name,
            kind,
            size: metadata.len(),
            mode: metadata.permissions().mode(),
            modified: metadata.modified().ok(),
            selected: false,
        })
    }

    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    pub fn permissions(&self) -> String {
        let mut text = String::with_capacity(10);
        text.push(match self.kind {
            EntryKind::Directory => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::BlockDevice => 'b',
            _ => '-',
        });
        for (bit, ch) in [
            (0o400, 'r'),
            (0o200, 'w'),
            (0o100, 'x'),
            (0o040, 'r'),
            (0o020, 'w'),
            (0o010, 'x'),
            (0o004, 'r'),
            (0o002, 'w'),
            (0o001, 'x'),
        ] {
            text.push(if self.mode & bit != 0 { ch } else { '-' });
        }
        text
    }

    pub fn modified_text(&self) -> String {
        self.modified
            .map(|time| {
                DateTime::<Local>::from(time)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| "—".into())
    }

    pub fn size_text(&self) -> String {
        if self.kind == EntryKind::Directory {
            "—".into()
        } else {
            human_size(self.size)
        }
    }
}

pub fn read_directory(
    path: &Path,
    show_hidden: bool,
    sort: SortSetting,
    reverse: bool,
    directories_first: bool,
) -> Result<Vec<FileEntry>> {
    let read = fs::read_dir(path).map_err(|error| {
        crate::error::io_error(format!("could not read {}", path.display()), error)
    })?;
    let mut entries = Vec::new();
    for item in read {
        let Ok(item) = item else { continue };
        let Ok(entry) = FileEntry::from_path(item.path()) else {
            continue;
        };
        if show_hidden || !entry.is_hidden() {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| compare(left, right, sort, directories_first));
    if reverse {
        entries.reverse();
    }
    Ok(entries)
}

fn compare(left: &FileEntry, right: &FileEntry, sort: SortSetting, dirs_first: bool) -> Ordering {
    if dirs_first {
        match (
            left.kind == EntryKind::Directory,
            right.kind == EntryKind::Directory,
        ) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }
    let order = match sort {
        SortSetting::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        SortSetting::Extension => left.path.extension().cmp(&right.path.extension()),
        SortSetting::Size => left.size.cmp(&right.size),
        SortSetting::Modified => left.modified.cmp(&right.modified),
        SortSetting::Type => left.kind.cmp(&right.kind),
        SortSetting::Permissions => left.mode.cmp(&right.mode),
    };
    order.then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
}

pub fn human_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_permissions() {
        let entry = FileEntry {
            path: "file".into(),
            name: "file".into(),
            kind: EntryKind::File,
            size: 0,
            mode: 0o100754,
            modified: None,
            selected: false,
        };
        assert_eq!(entry.permissions(), "-rwxr-xr--");
    }
}
