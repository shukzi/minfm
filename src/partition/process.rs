use super::*;

pub(super) fn authenticate_sudo(password: &[u8]) -> Result<PathBuf> {
    let sudo = trusted_program(&OsString::from("sudo"))?;
    let reset = Command::new(&sudo)
        .arg("--reset-timestamp")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| crate::error::io_error("could not reset sudo authentication", error))?;
    if !reset.success() {
        return Err(MinfmError::Message(
            "could not reset cached administrator authentication".into(),
        ));
    }
    let mut child = Command::new(&sudo)
        .args(["--stdin", "--prompt=", "--validate"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| crate::error::io_error("could not start sudo authentication", error))?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            MinfmError::Message("could not securely open the sudo password pipe".into())
        })?;
        stdin
            .write_all(password)
            .and_then(|()| stdin.write_all(b"\n"))
            .map_err(|error| crate::error::io_error("could not send the sudo password", error))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| crate::error::io_error("could not finish sudo authentication", error))?;
    if output.status.success() {
        return Ok(sudo);
    }
    let diagnostic = [output.stderr.as_slice(), output.stdout.as_slice()]
        .into_iter()
        .filter_map(|bytes| {
            let text = String::from_utf8_lossy(bytes).trim().replace('\n', " ");
            (!text.is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let lower = diagnostic.to_ascii_lowercase();
    if lower.contains("not in the sudoers")
        || lower.contains("not allowed to run sudo")
        || lower.contains("may not run sudo")
    {
        return Err(MinfmError::Message(format!(
            "administrator authorization was denied{}",
            if diagnostic.is_empty() {
                String::new()
            } else {
                format!(": {diagnostic}")
            }
        )));
    }
    Err(MinfmError::IncorrectPassphrase)
}

pub(super) fn run_command(
    spec: &CommandSpec,
    use_sudo: bool,
    administrator_password: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let requested_program = trusted_program(&spec.program)?;
    let elevated_with_sudo = spec.elevated && use_sudo;
    let (program, arguments) = if elevated_with_sudo {
        (
            trusted_program(&OsString::from("sudo"))?.into_os_string(),
            sudo_command_arguments(requested_program.into_os_string(), &spec.arguments),
        )
    } else {
        (requested_program.into_os_string(), spec.arguments.clone())
    };
    let mut child = Command::new(&program)
        .args(&arguments)
        .stdin(if elevated_with_sudo {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            crate::error::io_error(
                format!("could not run {}", program.to_string_lossy()),
                error,
            )
        })?;
    if elevated_with_sudo {
        let password = administrator_password.ok_or_else(|| {
            MinfmError::Message("administrator authentication is required".into())
        })?;
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            MinfmError::Message("could not securely open the sudo password pipe".into())
        })?;
        stdin
            .write_all(password)
            .and_then(|()| stdin.write_all(b"\n"))
            .map_err(|error| {
                crate::error::io_error("could not send the administrator password", error)
            })?;
    }
    let output = child.wait_with_output().map_err(|error| {
        crate::error::io_error(
            format!("could not finish {}", program.to_string_lossy()),
            error,
        )
    })?;
    let code = output.status.code().unwrap_or(-1);
    if spec.accepted_codes.contains(&code) {
        return Ok(output.stdout);
    }
    let diagnostic = [output.stderr.as_slice(), output.stdout.as_slice()]
        .into_iter()
        .filter_map(|bytes| {
            let text = String::from_utf8_lossy(bytes).trim().replace('\n', " ");
            (!text.is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join(" · ");
    Err(MinfmError::Message(format!(
        "{} failed{}",
        spec.program.to_string_lossy(),
        if diagnostic.is_empty() {
            format!(" with status {}", output.status)
        } else {
            format!(": {diagnostic}")
        }
    )))
}

pub(super) fn run_command_with_secret<const N: usize>(
    program: &str,
    arguments: [OsString; N],
    secret: &[u8],
    use_sudo: bool,
) -> Result<()> {
    let requested = trusted_program(&OsString::from(program))?;
    let (executable, arguments) = if use_sudo {
        let sudo = trusted_program(&OsString::from("sudo"))?;
        let mut elevated = vec![
            OsString::from("--non-interactive"),
            OsString::from("--"),
            requested.into_os_string(),
        ];
        elevated.extend(arguments);
        (sudo.into_os_string(), elevated)
    } else {
        (requested.into_os_string(), arguments.into_iter().collect())
    };
    let mut child = Command::new(&executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| crate::error::io_error(format!("could not run {program}"), error))?;
    child
        .stdin
        .take()
        .ok_or_else(|| MinfmError::Message("could not open the encryption key pipe".into()))?
        .write_all(secret)
        .map_err(|error| crate::error::io_error("could not send the encryption key", error))?;
    let output = child
        .wait_with_output()
        .map_err(|error| crate::error::io_error(format!("could not finish {program}"), error))?;
    if output.status.success() {
        Ok(())
    } else {
        let diagnostic = String::from_utf8_lossy(&output.stderr)
            .trim()
            .replace('\n', " ");
        Err(MinfmError::Message(format!(
            "{program} failed{}",
            if diagnostic.is_empty() {
                format!(" with status {}", output.status)
            } else {
                format!(": {diagnostic}")
            }
        )))
    }
}

pub(super) fn write_new_backup(destination: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            crate::error::io_error("could not create partition-table backup", error)
        })?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| crate::error::io_error("could not save partition-table backup", error))
}

pub(super) fn sudo_command_arguments(program: OsString, arguments: &[OsString]) -> Vec<OsString> {
    let mut sudo_arguments = Vec::with_capacity(arguments.len() + 5);
    sudo_arguments.push(OsString::from("--stdin"));
    sudo_arguments.push(OsString::from("--reset-timestamp"));
    sudo_arguments.push(OsString::from("--prompt="));
    sudo_arguments.push(OsString::from("--"));
    sudo_arguments.push(program);
    sudo_arguments.extend(arguments.iter().cloned());
    sudo_arguments
}

pub(super) fn trusted_program(program: &OsString) -> Result<PathBuf> {
    let program_name = PathBuf::from(program);
    if program_name.components().count() != 1 {
        return Err(MinfmError::Message(
            "partition helper names must not contain paths".into(),
        ));
    }
    let mut directories = vec![
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/bin"),
    ];
    if let Some(path) = env::var_os("PATH") {
        directories.extend(env::split_paths(&path));
    }
    directories.sort();
    directories.dedup();
    for directory in directories {
        let candidate = directory.join(&program_name);
        let Ok(canonical) = fs::canonicalize(&candidate) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&canonical) else {
            continue;
        };
        if metadata.is_file()
            && metadata.uid() == 0
            && metadata.mode() & 0o022 == 0
            && trusted_ownership_chain(&canonical)
        {
            return Ok(canonical);
        }
    }
    Err(MinfmError::Message(format!(
        "required trusted system tool {} is missing",
        program.to_string_lossy()
    )))
}

pub(super) fn trusted_ownership_chain(path: &Path) -> bool {
    path.ancestors().skip(1).all(|ancestor| {
        fs::metadata(ancestor)
            .is_ok_and(|metadata| metadata.uid() == 0 && metadata.mode() & 0o022 == 0)
    })
}

pub(super) fn settle_devices() {
    let _ = Command::new("udevadm")
        .args(["settle", "--timeout=10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
