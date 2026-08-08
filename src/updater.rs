use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const REPOSITORY: &str = "shukzi/minfm";
const BINARY_ASSET: &str = "minfm-linux-x86_64";

#[derive(Debug)]
pub enum CheckOutcome {
    Current,
    Available { version: String },
    Unavailable,
}

pub fn check(current: &str) -> CheckOutcome {
    if !command_available("curl") {
        return CheckOutcome::Unavailable;
    }
    let url = format!("https://github.com/{REPOSITORY}/releases/latest");
    let output = match Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "8",
            "-fsSL",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            &url,
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return CheckOutcome::Unavailable,
    };
    let effective_url = String::from_utf8_lossy(&output.stdout);
    let Some(tag) = effective_url.trim().rsplit('/').next() else {
        return CheckOutcome::Unavailable;
    };
    let Some(latest) = parse_version_tag(tag) else {
        return CheckOutcome::Unavailable;
    };
    let Some(current) = parse_version(current) else {
        return CheckOutcome::Unavailable;
    };
    if latest > current {
        CheckOutcome::Available {
            version: tag.to_string(),
        }
    } else {
        CheckOutcome::Current
    }
}

pub fn install(version: &str, executable: &Path) -> Result<(), String> {
    if std::env::consts::ARCH != "x86_64" {
        return Err(format!(
            "No published updater binary is available for {}",
            std::env::consts::ARCH
        ));
    }
    if parse_version_tag(version).is_none() {
        return Err("The release version returned by GitHub is invalid".into());
    }
    if !command_available("curl") || !command_available("sha256sum") {
        return Err("Updating requires curl and sha256sum".into());
    }
    let parent = executable
        .parent()
        .ok_or_else(|| "The installed binary has no parent directory".to_string())?;
    let (temporary, checksum) = unique_temporary_paths(parent)?;
    let result = install_inner(version, executable, &temporary, &checksum);
    let _ = fs::remove_file(&temporary);
    let _ = fs::remove_file(&checksum);
    result
}

fn install_inner(
    version: &str,
    executable: &Path,
    temporary: &Path,
    checksum: &Path,
) -> Result<(), String> {
    let release = format!("https://github.com/{REPOSITORY}/releases/download/{version}");
    download(
        &format!("{release}/{BINARY_ASSET}"),
        temporary,
        Duration::from_secs(120),
    )?;
    download(
        &format!("{release}/{BINARY_ASSET}.sha256"),
        checksum,
        Duration::from_secs(30),
    )?;
    verify_checksum(temporary, checksum)?;
    fs::set_permissions(temporary, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("Could not set update permissions: {error}"))?;
    File::open(temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Could not sync the downloaded update: {error}"))?;
    fs::rename(temporary, executable)
        .map_err(|error| format!("Could not replace {}: {error}", executable.display()))?;
    if let Some(parent) = executable.parent() {
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }
    Ok(())
}

fn download(url: &str, destination: &Path, timeout: Duration) -> Result<(), String> {
    let timeout = timeout.as_secs().to_string();
    let status = Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            &timeout,
            "-fsSL",
            url,
            "-o",
        ])
        .arg(destination)
        .status()
        .map_err(|error| format!("Could not start curl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Download failed with status {status}"))
    }
}

fn verify_checksum(binary: &Path, checksum: &Path) -> Result<(), String> {
    let mut checksum_text = String::new();
    File::open(checksum)
        .and_then(|mut file| file.read_to_string(&mut checksum_text))
        .map_err(|error| format!("Could not read the release checksum: {error}"))?;
    let expected = checksum_text
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "The release checksum has an invalid format".to_string())?;
    let output = Command::new("sha256sum")
        .arg(binary)
        .output()
        .map_err(|error| format!("Could not start sha256sum: {error}"))?;
    if !output.status.success() {
        return Err("Could not calculate the update checksum".into());
    }
    let actual = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err("The downloaded update failed checksum verification".into())
    }
}

fn unique_temporary_paths(parent: &Path) -> Result<(PathBuf, PathBuf), String> {
    for counter in 0..1000u32 {
        let binary = parent.join(format!(".minfm-update-{}-{counter}", std::process::id()));
        let checksum = parent.join(format!(
            ".minfm-update-{}-{counter}.sha256",
            std::process::id()
        ));
        match create_temporary_file(&binary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not create update file: {error}")),
        }
        match create_temporary_file(&checksum) {
            Ok(()) => return Ok((binary, checksum)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&binary);
            }
            Err(error) => {
                let _ = fs::remove_file(&binary);
                return Err(format!("Could not create checksum file: {error}"));
            }
        }
    }
    Err("Could not allocate temporary update files".into())
}

fn create_temporary_file(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&[])
}

fn command_available(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
        })
        .unwrap_or(false)
}

fn parse_version_tag(tag: &str) -> Option<(u64, u64, u64)> {
    parse_version(tag.strip_prefix('v')?)
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_strict_and_orderable() {
        assert_eq!(parse_version_tag("v1.2.3"), Some((1, 2, 3)));
        assert!(parse_version_tag("1.2.3").is_none());
        assert!(parse_version_tag("v1.2").is_none());
        assert!(parse_version_tag("v1.2.3/asset").is_none());
        assert!(parse_version("0.1.3") > parse_version("0.1.2"));
    }

    #[test]
    fn checksum_verification_accepts_only_matching_bytes() {
        if !command_available("sha256sum") {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("minfm");
        let checksum = temp.path().join("minfm.sha256");
        fs::write(&binary, b"safe update bytes").unwrap();
        let output = Command::new("sha256sum").arg(&binary).output().unwrap();
        fs::write(&checksum, output.stdout).unwrap();

        assert!(verify_checksum(&binary, &checksum).is_ok());

        fs::write(&checksum, format!("{}  minfm\n", "0".repeat(64))).unwrap();
        assert!(verify_checksum(&binary, &checksum).is_err());
    }

    #[test]
    fn update_temporary_files_are_created_exclusively() {
        let temp = tempfile::tempdir().unwrap();
        let (binary, checksum) = unique_temporary_paths(temp.path()).unwrap();

        assert!(binary.is_file());
        assert!(checksum.is_file());
        assert_ne!(binary, checksum);
    }
}
