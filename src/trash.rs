use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use nix::unistd::Uid;
use percent_encoding::{percent_decode, percent_encode, NON_ALPHANUMERIC};

use crate::{
    error::{io_error, MinfmError, Result},
    safety,
};

const DATE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Debug, Clone)]
pub struct TrashEntry {
    pub name: OsString,
    pub trashed_path: PathBuf,
    pub info_path: PathBuf,
    pub original_path: PathBuf,
    pub deleted_at: DateTime<Local>,
}

impl TrashEntry {
    pub fn display_name(&self) -> String {
        self.name.to_string_lossy().into_owned()
    }

    pub fn deleted_text(&self) -> String {
        self.deleted_at.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    pub fn estimated_size(&self) -> u64 {
        estimated_size(&self.trashed_path)
    }
}

fn estimated_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| estimated_size(&entry.path()))
                    .sum()
            })
            .unwrap_or(0);
    }
    metadata.len()
}

#[derive(Debug, Clone)]
pub struct TrashManager {
    root: PathBuf,
    files: PathBuf,
    info: PathBuf,
}

impl TrashManager {
    #[cfg(test)]
    pub(crate) fn isolated(root: &Path) -> Self {
        let root = root.join("trash");
        let files = root.join("files");
        let info = root.join("info");
        fs::create_dir_all(&files).unwrap();
        fs::create_dir_all(&info).unwrap();
        Self { root, files, info }
    }

    pub fn for_path(path: &Path) -> Result<Self> {
        let root = trash_root_for(path)?;
        let files = root.join("files");
        let info = root.join("info");
        fs::create_dir_all(&files)
            .map_err(|error| io_error(format!("could not create {}", files.display()), error))?;
        fs::create_dir_all(&info)
            .map_err(|error| io_error(format!("could not create {}", info.display()), error))?;
        let private = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&root, private.clone()).ok();
        fs::set_permissions(&files, private.clone()).ok();
        fs::set_permissions(&info, private).ok();
        Ok(Self { root, files, info })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn move_to_trash(
        &self,
        source: &Path,
        current_dir: &Path,
        config_dir: &Path,
    ) -> Result<TrashEntry> {
        safety::ensure_trashable(source, current_dir, config_dir)?;
        let original = fs::canonicalize(source)
            .map_err(|error| io_error(format!("could not resolve {}", source.display()), error))?;
        let base_name = original.file_name().ok_or_else(|| {
            MinfmError::Message(format!("{} has no filename", original.display()))
        })?;
        let name = self.unique_name(base_name);
        let trashed_path = self.files.join(&name);
        let info_path = self.info.join(info_filename(&name));
        let deleted_at = Local::now();
        let metadata = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode_path(&original),
            deleted_at.format(DATE_FORMAT)
        );

        fs::write(&info_path, metadata)
            .map_err(|error| io_error(format!("could not write {}", info_path.display()), error))?;
        if let Err(error) = fs::rename(&original, &trashed_path) {
            let _ = fs::remove_file(&info_path);
            return Err(io_error(
                format!("could not move {} to trash", original.display()),
                error,
            ));
        }
        Ok(TrashEntry {
            name,
            trashed_path,
            info_path,
            original_path: original,
            deleted_at,
        })
    }

    pub fn list(&self) -> Result<Vec<TrashEntry>> {
        let mut entries = Vec::new();
        let read = fs::read_dir(&self.info)
            .map_err(|error| io_error(format!("could not read {}", self.info.display()), error))?;
        for item in read.flatten() {
            let path = item.path();
            if path.extension() != Some(OsStr::new("trashinfo")) {
                continue;
            }
            if let Ok(entry) = self.parse_info(&path) {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.deleted_at));
        Ok(entries)
    }

    pub fn restore(&self, entry: &TrashEntry, destination: Option<&Path>) -> Result<PathBuf> {
        let target = destination
            .map(Path::to_path_buf)
            .unwrap_or_else(|| entry.original_path.clone());
        if target.exists() {
            return Err(MinfmError::DestinationExists(target));
        }
        let parent = target.parent().ok_or_else(|| {
            MinfmError::Message(format!("{} has no parent directory", target.display()))
        })?;
        if !parent.is_dir() {
            return Err(MinfmError::Message(format!(
                "restore directory does not exist: {}",
                parent.display()
            )));
        }
        fs::rename(&entry.trashed_path, &target)
            .map_err(|error| io_error(format!("could not restore {}", target.display()), error))?;
        if let Err(error) = fs::remove_file(&entry.info_path) {
            let rollback = fs::rename(&target, &entry.trashed_path);
            return Err(MinfmError::Message(format!(
                "restored data but could not remove trash metadata ({error}); rollback: {}",
                if rollback.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                }
            )));
        }
        Ok(target)
    }

    pub fn permanently_delete(&self, entry: &TrashEntry) -> Result<()> {
        if !entry.trashed_path.starts_with(&self.files) || !entry.info_path.starts_with(&self.info)
        {
            return Err(MinfmError::Message(
                "refusing to permanently delete an item outside this trash directory".into(),
            ));
        }
        let metadata = fs::symlink_metadata(&entry.trashed_path).map_err(|error| {
            io_error(
                format!("could not inspect {}", entry.trashed_path.display()),
                error,
            )
        })?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&entry.trashed_path).map_err(|error| {
                io_error(
                    format!(
                        "could not permanently delete {}",
                        entry.trashed_path.display()
                    ),
                    error,
                )
            })?;
        } else {
            fs::remove_file(&entry.trashed_path).map_err(|error| {
                io_error(
                    format!(
                        "could not permanently delete {}",
                        entry.trashed_path.display()
                    ),
                    error,
                )
            })?;
        }
        fs::remove_file(&entry.info_path).map_err(|error| {
            io_error(
                format!(
                    "data was deleted, but trash metadata could not be removed: {}",
                    entry.info_path.display()
                ),
                error,
            )
        })?;
        Ok(())
    }

    fn unique_name(&self, base: &OsStr) -> OsString {
        if !self.files.join(base).exists() && !self.info.join(info_filename(base)).exists() {
            return base.to_os_string();
        }
        let base_bytes = base.as_bytes();
        for counter in 1u64.. {
            let mut bytes = base_bytes.to_vec();
            bytes.extend_from_slice(format!(".{counter}").as_bytes());
            let candidate = OsString::from_vec(bytes);
            if !self.files.join(&candidate).exists()
                && !self.info.join(info_filename(&candidate)).exists()
            {
                return candidate;
            }
        }
        unreachable!()
    }

    fn parse_info(&self, info_path: &Path) -> Result<TrashEntry> {
        let text = fs::read_to_string(info_path)
            .map_err(|error| io_error(format!("could not read {}", info_path.display()), error))?;
        let encoded_path = text.lines().find_map(|line| line.strip_prefix("Path="));
        let deleted = text
            .lines()
            .find_map(|line| line.strip_prefix("DeletionDate="));
        let (Some(encoded_path), Some(deleted)) = (encoded_path, deleted) else {
            return Err(MinfmError::InvalidTrashInfo(info_path.to_path_buf()));
        };
        let original_path = decode_path(encoded_path);
        let naive = NaiveDateTime::parse_from_str(deleted, DATE_FORMAT)
            .map_err(|_| MinfmError::InvalidTrashInfo(info_path.to_path_buf()))?;
        let deleted_at = Local
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| MinfmError::InvalidTrashInfo(info_path.to_path_buf()))?;
        let stem = info_path
            .file_stem()
            .ok_or_else(|| MinfmError::InvalidTrashInfo(info_path.to_path_buf()))?
            .to_os_string();
        Ok(TrashEntry {
            name: stem.clone(),
            trashed_path: self.files.join(&stem),
            info_path: info_path.to_path_buf(),
            original_path,
            deleted_at,
        })
    }
}

fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn trash_root_for(path: &Path) -> Result<PathBuf> {
    let source_meta = fs::metadata(path)
        .map_err(|error| io_error(format!("could not inspect {}", path.display()), error))?;
    let home_trash = data_home().join("Trash");
    let comparison = home_trash.parent().unwrap_or(Path::new("/tmp"));
    if let Ok(home_meta) = fs::metadata(comparison) {
        if source_meta.dev() == home_meta.dev() {
            return Ok(home_trash);
        }
    }
    let mount_root = find_mount_root(path)?;
    Ok(mount_root.join(format!(".Trash-{}", Uid::effective().as_raw())))
}

fn find_mount_root(path: &Path) -> Result<PathBuf> {
    let mut current = fs::canonicalize(path)
        .map_err(|error| io_error(format!("could not resolve {}", path.display()), error))?;
    if !current.is_dir() {
        current.pop();
    }
    loop {
        let Some(parent) = current.parent() else {
            return Ok(current);
        };
        let current_dev = fs::metadata(&current)
            .map_err(|error| io_error("mount check", error))?
            .dev();
        let parent_dev = fs::metadata(parent)
            .map_err(|error| io_error("mount check", error))?
            .dev();
        if current_dev != parent_dev || parent == current {
            return Ok(current);
        }
        current = parent.to_path_buf();
    }
}

fn info_filename(name: &OsStr) -> OsString {
    let mut bytes = name.as_bytes().to_vec();
    bytes.extend_from_slice(b".trashinfo");
    OsString::from_vec(bytes)
}

fn encode_path(path: &Path) -> String {
    percent_encode(path.as_os_str().as_bytes(), NON_ALPHANUMERIC).to_string()
}

fn decode_path(value: &str) -> PathBuf {
    let bytes: Vec<u8> = percent_decode(value.as_bytes()).collect();
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_manager(root: &Path) -> TrashManager {
        TrashManager::isolated(root)
    }

    #[test]
    fn trash_and_restore_preserves_contents_and_exact_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let source = workspace.join("important.txt");
        fs::write(&source, b"original bytes").unwrap();
        let manager = isolated_manager(temp.path());
        let entry = manager
            .move_to_trash(
                &source,
                &workspace.join("elsewhere"),
                &temp.path().join("config"),
            )
            .unwrap();
        assert!(!source.exists());
        assert_eq!(entry.deleted_text().len(), 19);
        let restored = manager.restore(&entry, None).unwrap();
        assert_eq!(fs::read(restored).unwrap(), b"original bytes");
    }

    #[test]
    fn restore_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let source = workspace.join("same.txt");
        fs::write(&source, b"first").unwrap();
        let manager = isolated_manager(temp.path());
        let entry = manager
            .move_to_trash(
                &source,
                &workspace.join("elsewhere"),
                &temp.path().join("config"),
            )
            .unwrap();
        fs::write(&source, b"replacement").unwrap();
        assert!(matches!(
            manager.restore(&entry, None),
            Err(MinfmError::DestinationExists(_))
        ));
        assert_eq!(fs::read(source).unwrap(), b"replacement");
    }

    #[test]
    fn permanent_delete_is_confined_to_trash() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let source = workspace.join("gone.txt");
        fs::write(&source, b"delete me").unwrap();
        let manager = isolated_manager(temp.path());
        let entry = manager
            .move_to_trash(
                &source,
                &workspace.join("elsewhere"),
                &temp.path().join("config"),
            )
            .unwrap();
        manager.permanently_delete(&entry).unwrap();
        assert!(!entry.trashed_path.exists());
        assert!(!entry.info_path.exists());

        let outside = temp.path().join("outside.txt");
        fs::write(&outside, b"must survive").unwrap();
        let forged = TrashEntry {
            name: OsString::from("outside.txt"),
            trashed_path: outside.clone(),
            info_path: temp.path().join("outside.trashinfo"),
            original_path: outside.clone(),
            deleted_at: Local::now(),
        };
        assert!(manager.permanently_delete(&forged).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"must survive");
    }

    #[test]
    fn estimates_nested_trash_size_without_following_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("directory");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("one"), b"1234").unwrap();
        fs::write(directory.join("two"), b"56789").unwrap();

        assert_eq!(estimated_size(&directory), 9);
    }
}
