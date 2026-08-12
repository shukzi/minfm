use std::{
    env, fmt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::SyncSender,
    thread,
};

#[derive(Debug, Clone)]
pub struct LaunchError {
    pub program: String,
    pub path: PathBuf,
    pub detail: String,
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Could not open {} with {}.\n\n{}",
            self.path.display(),
            self.program,
            self.detail
        )
    }
}

pub fn resolve_editor(configured: &str) -> String {
    if configured == "$EDITOR" {
        env::var("EDITOR").unwrap_or_else(|_| "xdg-open".into())
    } else {
        configured.into()
    }
}

pub fn launch<E: Send + 'static>(
    program: String,
    path: PathBuf,
    errors: SyncSender<E>,
    wrap_error: impl Fn(LaunchError) -> E + Send + 'static,
) -> Result<(), LaunchError> {
    if program.trim().is_empty() {
        return Err(LaunchError {
            program: "configured application".into(),
            path,
            detail: "The configured command is empty.".into(),
        });
    }

    let mut command = Command::new(&program);
    command
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| LaunchError {
        program: program.clone(),
        path: path.clone(),
        detail: error.to_string(),
    })?;
    thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let _ = errors.try_send(wrap_error(LaunchError {
                program,
                path,
                detail: format!("The application exited with {status}."),
            }));
        }
        Err(error) => {
            let _ = errors.try_send(wrap_error(LaunchError {
                program,
                path,
                detail: format!("Could not monitor the application: {error}"),
            }));
        }
    });
    Ok(())
}

pub fn run_terminal_editor(program: &str, path: &Path) -> Result<(), LaunchError> {
    if program.trim().is_empty() {
        return Err(LaunchError {
            program: "configured editor".into(),
            path: path.to_path_buf(),
            detail: "The configured command is empty.".into(),
        });
    }
    let status = Command::new(program)
        .arg(path)
        .status()
        .map_err(|error| LaunchError {
            program: program.into(),
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(LaunchError {
            program: program.into(),
            path: path.to_path_buf(),
            detail: format!("The editor exited with {status}."),
        })
    }
}

pub fn is_terminal_editor(program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "vi" | "vim" | "nvim" | "nano" | "hx" | "helix" | "micro" | "kak" | "kakoune"
    )
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    #[test]
    fn recognizes_known_terminal_editors_by_name_or_path() {
        assert!(is_terminal_editor("nano"));
        assert!(is_terminal_editor("/usr/bin/nvim"));
        assert!(!is_terminal_editor("xdg-open"));
        assert!(!is_terminal_editor("gedit"));
    }

    #[test]
    fn successful_direct_launch_is_silent() {
        let (sender, receiver) = mpsc::sync_channel(1);
        launch(
            "/bin/true".into(),
            PathBuf::from("example.txt"),
            sender,
            |error| error,
        )
        .unwrap();
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn failed_direct_launch_is_reported() {
        let (sender, receiver) = mpsc::sync_channel(1);
        launch(
            "/bin/false".into(),
            PathBuf::from("example.txt"),
            sender,
            |error| error,
        )
        .unwrap();
        let error = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(error.program, "/bin/false");
        assert!(error.detail.contains("exited with"));
    }

    #[test]
    fn missing_direct_application_fails_immediately() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let error = launch(
            "/minfm-test/missing-opener".into(),
            PathBuf::from("example.txt"),
            sender,
            |error| error,
        )
        .unwrap_err();
        assert!(error.detail.contains("No such file") || error.detail.contains("not found"));
    }

    #[test]
    fn terminal_editor_exit_status_is_checked() {
        assert!(run_terminal_editor("/bin/true", Path::new("example.txt")).is_ok());
        let error = run_terminal_editor("/bin/false", Path::new("example.txt")).unwrap_err();
        assert!(error.detail.contains("editor exited"));
    }
}
