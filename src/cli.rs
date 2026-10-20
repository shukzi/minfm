use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub force_read_only: bool,
    pub start: PathBuf,
}

pub fn parse(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<Options, Box<dyn std::error::Error>> {
    let mut force_read_only = false;
    let mut path = None;
    for argument in arguments {
        if argument == "--read-only" {
            force_read_only = true;
        } else if argument == "--version" || argument == "-V" {
            println!("minfm {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        } else if argument == "--help" || argument == "-h" {
            println!("minfm [--read-only] [path]");
            std::process::exit(0);
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            return Err("only one starting path may be provided".into());
        }
    }
    let path = path.unwrap_or(std::env::current_dir()?);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()).into());
    }
    Ok(Options {
        force_read_only,
        start: path.canonicalize()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_read_only_and_canonical_start_path() {
        let root = tempfile::tempdir().unwrap();
        let options = parse([
            OsString::from("--read-only"),
            root.path().as_os_str().to_owned(),
        ])
        .unwrap();
        assert!(options.force_read_only);
        assert_eq!(options.start, root.path().canonicalize().unwrap());
    }

    #[test]
    fn rejects_multiple_paths_and_non_directories() {
        let root = tempfile::tempdir().unwrap();
        assert!(parse([
            root.path().as_os_str().to_owned(),
            root.path().as_os_str().to_owned(),
        ])
        .is_err());
        let file = root.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(parse([file.into_os_string()]).is_err());
    }
}
