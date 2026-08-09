use std::{
    ffi::OsString,
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
    pub(crate) fn from_dir_entry(item: fs::DirEntry) -> std::io::Result<Self> {
        let metadata = item.metadata()?;
        let path = item.path();
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
        let name = item.file_name().to_string_lossy().into_owned();
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

    pub fn is_text_file(&self) -> bool {
        if self.kind != EntryKind::File {
            return false;
        }
        let name = self.name.to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "readme"
                | "license"
                | "copying"
                | "makefile"
                | "dockerfile"
                | "cargo.lock"
                | ".gitignore"
                | ".gitattributes"
                | ".editorconfig"
        ) {
            return true;
        }
        self.path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "txt"
                        | "md"
                        | "markdown"
                        | "rst"
                        | "log"
                        | "csv"
                        | "tsv"
                        | "json"
                        | "jsonc"
                        | "toml"
                        | "yaml"
                        | "yml"
                        | "xml"
                        | "html"
                        | "htm"
                        | "css"
                        | "js"
                        | "jsx"
                        | "ts"
                        | "tsx"
                        | "rs"
                        | "c"
                        | "h"
                        | "cc"
                        | "cpp"
                        | "hpp"
                        | "sh"
                        | "bash"
                        | "zsh"
                        | "fish"
                        | "py"
                        | "rb"
                        | "go"
                        | "java"
                        | "kt"
                        | "kts"
                        | "php"
                        | "lua"
                        | "sql"
                        | "ini"
                        | "cfg"
                        | "conf"
                        | "env"
                        | "desktop"
                        | "service"
                )
            })
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
        let Ok(entry) = FileEntry::from_dir_entry(item) else {
            continue;
        };
        if show_hidden || !entry.is_hidden() {
            entries.push(entry);
        }
    }
    sort_entries(&mut entries, sort, reverse, directories_first);
    Ok(entries)
}

pub(crate) fn sort_entries(
    entries: &mut [FileEntry],
    sort: SortSetting,
    reverse: bool,
    directories_first: bool,
) {
    let directory_rank = |entry: &FileEntry| {
        if directories_first && entry.kind != EntryKind::Directory {
            1_u8
        } else {
            0_u8
        }
    };
    match sort {
        SortSetting::Name => {
            entries.sort_by_cached_key(|entry| (directory_rank(entry), entry.name.to_lowercase()))
        }
        SortSetting::Extension => entries.sort_by_cached_key(|entry| {
            (
                directory_rank(entry),
                entry.path.extension().map(OsString::from),
                entry.name.to_lowercase(),
            )
        }),
        SortSetting::Size => entries.sort_by_cached_key(|entry| {
            (directory_rank(entry), entry.size, entry.name.to_lowercase())
        }),
        SortSetting::Modified => entries.sort_by_cached_key(|entry| {
            (
                directory_rank(entry),
                entry.modified,
                entry.name.to_lowercase(),
            )
        }),
        SortSetting::Type => entries.sort_by_cached_key(|entry| {
            (directory_rank(entry), entry.kind, entry.name.to_lowercase())
        }),
        SortSetting::Permissions => entries.sort_by_cached_key(|entry| {
            (directory_rank(entry), entry.mode, entry.name.to_lowercase())
        }),
    }
    if reverse {
        entries.reverse();
    }
}

pub fn contains_case_insensitive(haystack: &str, lowercase_needle: &str) -> bool {
    if lowercase_needle.is_empty() {
        return true;
    }
    if haystack.contains(lowercase_needle) {
        return true;
    }
    if haystack.is_ascii() && lowercase_needle.is_ascii() {
        return haystack
            .as_bytes()
            .windows(lowercase_needle.len())
            .any(|window| window.eq_ignore_ascii_case(lowercase_needle.as_bytes()));
    }
    haystack.to_lowercase().contains(lowercase_needle)
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
    use std::os::unix::fs::symlink;
    use std::time::Instant;

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

    #[test]
    fn finds_ascii_and_unicode_case_insensitively() {
        assert!(contains_case_insensitive("QuarterlyReport.TXT", "report"));
        assert!(contains_case_insensitive("RÉSUMÉ.txt", "résumé"));
        assert!(!contains_case_insensitive("notes.txt", "report"));
    }

    #[test]
    fn recognizes_text_files_for_contextual_editing() {
        let entry = |name: &str, kind| FileEntry {
            path: PathBuf::from(name),
            name: name.into(),
            kind,
            size: 0,
            mode: 0,
            modified: None,
            selected: false,
        };
        assert!(entry("notes.txt", EntryKind::File).is_text_file());
        assert!(entry("README", EntryKind::File).is_text_file());
        assert!(entry("main.rs", EntryKind::File).is_text_file());
        assert!(!entry("image.png", EntryKind::File).is_text_file());
        assert!(!entry("notes.txt", EntryKind::Directory).is_text_file());
    }

    #[test]
    fn directory_reader_does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        symlink(&target, temp.path().join("link")).unwrap();

        let entries = read_directory(temp.path(), true, SortSetting::Name, false, true).unwrap();
        let link = entries.iter().find(|entry| entry.name == "link").unwrap();

        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[test]
    #[ignore]
    fn benchmark_large_directory_read_and_sort() {
        let path = std::env::var_os("MINFM_PERF_LARGE_DIR")
            .map(PathBuf::from)
            .expect("MINFM_PERF_LARGE_DIR is required");
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            let entries = read_directory(&path, true, SortSetting::Name, false, true).unwrap();
            assert_eq!(entries.len(), 20_000);
            samples.push(started.elapsed());
        }
        samples.sort();
        eprintln!("PERF directory_median_us={}", samples[4].as_micros());
    }
}
