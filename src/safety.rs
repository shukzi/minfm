use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::error::{MinfmError, Result};

const PROTECTED: &[&str] = &[
    "/", "/boot", "/dev", "/etc", "/home", "/lib", "/lib64", "/media", "/mnt", "/opt", "/proc",
    "/root", "/run", "/sbin", "/sys", "/tmp", "/usr", "/var",
];

pub fn canonical_for_check(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| {
        crate::error::io_error(format!("could not resolve {}", path.display()), error)
    })
}

pub fn ensure_trashable(path: &Path, current_dir: &Path, config_dir: &Path) -> Result<()> {
    let canonical = canonical_for_check(path)?;
    let current = fs::canonicalize(current_dir).unwrap_or_else(|_| current_dir.to_path_buf());
    let config = fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let binary_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|path| fs::canonicalize(path).ok());

    if PROTECTED.iter().any(|item| canonical == Path::new(item))
        || canonical == current
        || canonical == config
        || binary_dir.as_ref().is_some_and(|path| canonical == *path)
        || is_mount_root(&canonical)
    {
        return Err(MinfmError::ProtectedPath(canonical));
    }
    Ok(())
}

pub fn ensure_no_overlap(source: &Path, destination: &Path) -> Result<()> {
    let source = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let resolved_destination = fs::canonicalize(destination).ok();
    let destination_parent = destination.parent().unwrap_or(destination);
    let destination_parent =
        fs::canonicalize(destination_parent).unwrap_or_else(|_| destination_parent.to_path_buf());
    if resolved_destination
        .as_ref()
        .is_some_and(|path| *path == source)
        || destination_parent == source
        || destination_parent.starts_with(&source)
    {
        return Err(MinfmError::PathOverlap(destination.to_path_buf()));
    }
    Ok(())
}

pub fn is_mount_root(path: &Path) -> bool {
    if path == Path::new("/") {
        return true;
    }
    let Some(parent) = path.parent() else {
        return true;
    };
    match (fs::metadata(path), fs::metadata(parent)) {
        (Ok(item), Ok(parent)) => item.dev() != parent.dev(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_always_protected() {
        assert!(
            ensure_trashable(Path::new("/"), Path::new("/tmp/a"), Path::new("/tmp/b")).is_err()
        );
    }

    #[test]
    fn rejects_copying_directory_into_itself() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        assert!(ensure_no_overlap(&source, &source.join("nested")).is_err());
    }
}
