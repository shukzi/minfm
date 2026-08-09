use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{compiler_fence, Ordering},
    thread,
    time::Duration,
};

use crate::error::{MinfmError, Result};

#[derive(Debug, Clone)]
pub struct LuksDevice {
    pub source: PathBuf,
    pub drive: PathBuf,
    pub label: Option<String>,
    pub size: u64,
    pub mapping: Option<PathBuf>,
    pub mountpoints: Vec<PathBuf>,
    pub system_protected: bool,
    pub ejectable: bool,
    pub eject_blocked: bool,
}

impl LuksDevice {
    pub fn is_locked(&self) -> bool {
        self.mapping.is_none()
    }

    pub fn is_mounted(&self) -> bool {
        !self.mountpoints.is_empty()
    }

    pub fn state_text(&self) -> &'static str {
        if self.is_locked() {
            "locked"
        } else if self.is_mounted() {
            "mounted"
        } else {
            "unlocked"
        }
    }
}

#[derive(Clone, Default)]
pub struct SecretInput(Vec<u8>);

impl SecretInput {
    pub fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.0
            .extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        encoded.fill(0);
    }

    pub fn pop(&mut self) {
        if let Ok(text) = std::str::from_utf8(&self.0) {
            if let Some((index, _)) = text.char_indices().next_back() {
                self.0.truncate(index);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn character_count(&self) -> usize {
        std::str::from_utf8(&self.0)
            .map(|text| text.chars().count())
            .unwrap_or(0)
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretInput([REDACTED])")
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub enum LuksAction {
    UnlockAndMount {
        source: PathBuf,
        passphrase: SecretInput,
    },
    Mount {
        mapping: PathBuf,
    },
    UnmountAndLock {
        source: PathBuf,
        mapping: PathBuf,
    },
    Eject {
        source: PathBuf,
        drive: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct LuksOutcome {
    pub message: String,
    pub mountpoint: Option<PathBuf>,
}

#[derive(Debug)]
struct BlockRecord {
    path: PathBuf,
    kind: String,
    filesystem: String,
    parent: String,
    label: Option<String>,
    size: u64,
    mountpoints: Vec<PathBuf>,
    removable: bool,
    transport: String,
}

pub fn discover() -> Result<Vec<LuksDevice>> {
    let output = Command::new("lsblk")
        .args([
            "--pairs",
            "--bytes",
            "--paths",
            "--output",
            "PATH,TYPE,FSTYPE,MOUNTPOINTS,SIZE,LABEL,PKNAME,RM,TRAN",
        ])
        .output()
        .map_err(|error| crate::error::io_error("could not run lsblk", error))?;
    if !output.status.success() {
        return Err(MinfmError::Message(format!(
            "lsblk failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let protected_sources = system_mount_sources();
    parse_lsblk_with_protected(&String::from_utf8_lossy(&output.stdout), &protected_sources)
}

pub fn execute_with_progress(
    action: &LuksAction,
    mut report_phase: impl FnMut(&'static str),
) -> Result<LuksOutcome> {
    report_phase("Checking device safety");
    ensure_action_allowed(action)?;
    report_phase("Checking device tools");
    ensure_udisksctl()?;
    match action {
        LuksAction::UnlockAndMount { source, passphrase } => {
            report_phase("Unlocking encrypted volume");
            run_udisks_unlock(source, passphrase)?;
            report_phase("Waiting for unlocked volume");
            let mapping =
                wait_for_device(source, |device| device.mapping.clone()).ok_or_else(|| {
                    MinfmError::Message(
                        "volume unlocked, but its mapping was not discovered".into(),
                    )
                })?;
            report_phase("Mounting volume");
            run_udisks(["mount", "--block-device"], &mapping)?;
            report_phase("Confirming mount");
            let mountpoint = wait_for_device(source, |device| device.mountpoints.first().cloned());
            Ok(LuksOutcome {
                message: format!("Unlocked and mounted {}", source.display()),
                mountpoint,
            })
        }
        LuksAction::Mount { mapping } => {
            report_phase("Mounting volume");
            run_udisks(["mount", "--block-device"], mapping)?;
            report_phase("Confirming mount");
            let mountpoint = discover()
                .ok()
                .and_then(|devices| {
                    devices
                        .into_iter()
                        .find(|device| device.mapping.as_ref() == Some(mapping))
                })
                .and_then(|device| device.mountpoints.first().cloned());
            Ok(LuksOutcome {
                message: format!("Mounted {}", mapping.display()),
                mountpoint,
            })
        }
        LuksAction::UnmountAndLock { source, mapping } => {
            report_phase("Unmounting volume");
            run_udisks(["unmount", "--block-device"], mapping)?;
            report_phase("Locking encrypted volume");
            run_udisks(["lock", "--block-device"], source)?;
            Ok(LuksOutcome {
                message: format!("Unmounted and locked {}", source.display()),
                mountpoint: None,
            })
        }
        LuksAction::Eject { source, drive } => {
            report_phase("Checking drive state");
            let device = discover()?
                .into_iter()
                .find(|device| device.source == *source)
                .ok_or_else(|| {
                    MinfmError::Message("the selected volume is no longer available".into())
                })?;
            if device.system_protected || !device.ejectable {
                return Err(MinfmError::Message(
                    "the selected device cannot be safely ejected".into(),
                ));
            }
            if device.drive != *drive {
                return Err(MinfmError::Message(
                    "the physical drive changed before it could be ejected".into(),
                ));
            }
            if device.eject_blocked {
                return Err(MinfmError::Message(
                    "another volume on this drive is active; eject was cancelled".into(),
                ));
            }
            if let Some(mapping) = device.mapping.as_ref() {
                if device.is_mounted() {
                    report_phase("Unmounting volume");
                    run_udisks(["unmount", "--block-device"], mapping)?;
                }
                report_phase("Locking encrypted volume");
                run_udisks(["lock", "--block-device"], &device.source)?;
            }
            report_phase("Confirming drive state");
            let refreshed = discover()?
                .into_iter()
                .find(|candidate| candidate.source == *source)
                .ok_or_else(|| {
                    MinfmError::Message("the selected volume disappeared before eject".into())
                })?;
            if refreshed.system_protected
                || !refreshed.ejectable
                || refreshed.eject_blocked
                || refreshed.drive != *drive
            {
                return Err(MinfmError::Message(
                    "the drive state changed; eject was cancelled".into(),
                ));
            }
            report_phase("Ejecting device");
            run_udisks(["power-off", "--block-device"], drive)?;
            Ok(LuksOutcome {
                message: format!("Safely ejected {}", drive.display()),
                mountpoint: None,
            })
        }
    }
}

fn ensure_action_allowed(action: &LuksAction) -> Result<()> {
    let devices = discover()?;
    let device = match action {
        LuksAction::UnlockAndMount { source, .. }
        | LuksAction::UnmountAndLock { source, .. }
        | LuksAction::Eject { source, .. } => {
            devices.iter().find(|device| device.source == *source)
        }
        LuksAction::Mount { mapping } => devices
            .iter()
            .find(|device| device.mapping.as_ref() == Some(mapping)),
    };
    let device = device.ok_or_else(|| {
        MinfmError::Message("the selected encrypted volume is no longer available".into())
    })?;
    if device.system_protected {
        return Err(MinfmError::Message(
            "disk actions on a protected system device are disabled".into(),
        ));
    }
    if let LuksAction::Eject { drive, .. } = action {
        if !device.ejectable || device.drive != *drive {
            return Err(MinfmError::Message(
                "the selected device is not a removable drive that can be ejected".into(),
            ));
        }
        if device.eject_blocked {
            return Err(MinfmError::Message(
                "another volume on this drive is active; eject is unavailable".into(),
            ));
        }
    }
    Ok(())
}

fn system_mount_sources() -> Vec<PathBuf> {
    ["/", "/boot", "/boot/efi", "/usr", "/var"]
        .iter()
        .filter_map(|target| {
            Command::new("findmnt")
                .args(["-n", "-o", "SOURCE", "--target", target])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    let source = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    (!source.is_empty() && source.starts_with('/')).then(|| PathBuf::from(source))
                })
        })
        .collect()
}

fn ensure_udisksctl() -> Result<()> {
    Command::new("udisksctl")
        .arg("--version")
        .output()
        .map(|_| ())
        .map_err(|error| {
            MinfmError::Message(format!(
                "udisksctl is required for safe LUKS integration: {error}"
            ))
        })
}

fn run_udisks<const N: usize>(prefix: [&str; N], device: &Path) -> Result<()> {
    let output = Command::new("udisksctl")
        .args(prefix)
        .arg(device)
        .arg("--no-user-interaction")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| crate::error::io_error("could not run udisksctl", error))?;
    if output.status.success() {
        Ok(())
    } else {
        if incorrect_passphrase(&output.stderr, &output.stdout) {
            return Err(MinfmError::IncorrectPassphrase);
        }
        Err(MinfmError::Message(format!(
            "udisksctl failed: {}",
            clean_command_error(&output.stderr, output.status)
        )))
    }
}

fn run_udisks_unlock(device: &Path, passphrase: &SecretInput) -> Result<()> {
    let mut child = Command::new("udisksctl")
        .args(["unlock", "--block-device"])
        .arg(device)
        .args(["--key-file", "/dev/stdin", "--no-user-interaction"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| crate::error::io_error("could not run udisksctl", error))?;
    child
        .stdin
        .take()
        .ok_or_else(|| MinfmError::Message("could not securely open the passphrase pipe".into()))?
        .write_all(passphrase.expose())
        .map_err(|error| crate::error::io_error("could not send the passphrase", error))?;
    let output = child
        .wait_with_output()
        .map_err(|error| crate::error::io_error("could not wait for udisksctl", error))?;
    if output.status.success() {
        Ok(())
    } else {
        if incorrect_passphrase(&output.stderr, &output.stdout) {
            return Err(MinfmError::IncorrectPassphrase);
        }
        Err(MinfmError::Message(format!(
            "udisksctl failed: {}",
            clean_command_error(&output.stderr, output.status)
        )))
    }
}

fn incorrect_passphrase(stderr: &[u8], stdout: &[u8]) -> bool {
    let diagnostic = format!(
        "{} {}",
        String::from_utf8_lossy(stderr).to_lowercase(),
        String::from_utf8_lossy(stdout).to_lowercase(),
    );
    [
        "incorrect passphrase",
        "passphrase is incorrect",
        "wrong passphrase",
        "wrong-password",
        "no key available with this passphrase",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

fn clean_command_error(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let message = String::from_utf8_lossy(stderr).trim().replace('\n', " ");
    if message.is_empty() {
        status.to_string()
    } else {
        message
    }
}

fn wait_for_device<T>(source: &Path, value: impl Fn(&LuksDevice) -> Option<T>) -> Option<T> {
    for _ in 0..10 {
        if let Ok(devices) = discover() {
            if let Some(result) = devices
                .iter()
                .find(|device| device.source == source)
                .and_then(&value)
            {
                return Some(result);
            }
        }
        thread::sleep(Duration::from_millis(150));
    }
    None
}

#[cfg(test)]
fn parse_lsblk(text: &str) -> Result<Vec<LuksDevice>> {
    parse_lsblk_with_protected(text, &[])
}

fn parse_lsblk_with_protected(
    text: &str,
    protected_sources: &[PathBuf],
) -> Result<Vec<LuksDevice>> {
    let records = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_record)
        .collect::<Result<Vec<_>>>()?;

    let protected = protected_record_names(&records, protected_sources);
    let mut devices = Vec::new();
    for encrypted in records
        .iter()
        .filter(|record| record.filesystem == "crypto_LUKS")
    {
        let source_name = encrypted
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mapping = records.iter().find(|record| {
            record.kind == "crypt"
                && (record.parent == encrypted.path.to_string_lossy()
                    || record.parent == source_name)
        });
        let drive = physical_drive(&records, encrypted);
        let drive_record = records.iter().find(|record| record.path == drive);
        let ejectable =
            drive_record.is_some_and(|record| record.removable || record.transport == "usb");
        let eject_blocked = records.iter().any(|record| {
            record.path != encrypted.path
                && mapping.is_none_or(|mapping| record.path != mapping.path)
                && belongs_to_drive(&records, record, &drive)
                && (record.kind == "crypt" || !record.mountpoints.is_empty())
        });
        devices.push(LuksDevice {
            source: encrypted.path.clone(),
            drive,
            label: encrypted.label.clone(),
            size: encrypted.size,
            mapping: mapping.map(|record| record.path.clone()),
            mountpoints: mapping
                .map(|record| record.mountpoints.clone())
                .unwrap_or_default(),
            system_protected: protected.contains(&record_name(&encrypted.path))
                || mapping.is_some_and(|record| protected.contains(&record_name(&record.path))),
            ejectable,
            eject_blocked,
        });
    }
    devices.sort_by(|left, right| left.source.cmp(&right.source));
    Ok(devices)
}

fn physical_drive(records: &[BlockRecord], start: &BlockRecord) -> PathBuf {
    let mut current = start;
    while let Some(parent) = record_for_reference(records, &current.parent) {
        current = parent;
    }
    current.path.clone()
}

fn belongs_to_drive(records: &[BlockRecord], record: &BlockRecord, drive: &Path) -> bool {
    physical_drive(records, record) == drive
}

fn record_for_reference<'a>(
    records: &'a [BlockRecord],
    reference: &str,
) -> Option<&'a BlockRecord> {
    if reference.is_empty() {
        return None;
    }
    records.iter().find(|record| {
        record.path == Path::new(reference) || record_name(&record.path) == reference
    })
}

fn record_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn protected_record_names(records: &[BlockRecord], sources: &[PathBuf]) -> HashSet<String> {
    let mut protected = sources
        .iter()
        .flat_map(|source| [source.to_string_lossy().into_owned(), record_name(source)])
        .collect::<HashSet<_>>();

    let mut changed = true;
    while changed {
        changed = false;
        for record in records {
            let path = record.path.to_string_lossy().into_owned();
            let name = record_name(&record.path);
            if protected.contains(&path)
                || protected.contains(&name)
                || (!record.parent.is_empty() && protected.contains(&record.parent))
            {
                changed |= protected.insert(path);
                changed |= protected.insert(name);
                if !record.parent.is_empty() {
                    changed |= protected.insert(record.parent.clone());
                }
            }
        }
    }
    protected
}

fn parse_record(line: &str) -> Result<BlockRecord> {
    let fields = parse_pairs(line)?;
    let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
    let path = get("PATH");
    if path.is_empty() {
        return Err(MinfmError::Message(
            "lsblk returned a record without PATH".into(),
        ));
    }
    let mountpoints = get("MOUNTPOINTS")
        .lines()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .collect();
    let label = match get("LABEL") {
        value if value.is_empty() => None,
        value => Some(value),
    };
    Ok(BlockRecord {
        path: PathBuf::from(path),
        kind: get("TYPE"),
        filesystem: get("FSTYPE"),
        parent: get("PKNAME"),
        label,
        size: get("SIZE").parse().unwrap_or(0),
        mountpoints,
        removable: get("RM") == "1",
        transport: get("TRAN"),
    })
}

fn parse_pairs(line: &str) -> Result<HashMap<String, String>> {
    let bytes = line.as_bytes();
    let mut fields = HashMap::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' {
            index += 1;
        }
        if index >= bytes.len() || index + 1 >= bytes.len() || bytes[index + 1] != b'"' {
            return Err(MinfmError::Message(format!(
                "could not parse lsblk output: {line}"
            )));
        }
        let key = String::from_utf8_lossy(&bytes[key_start..index]).into_owned();
        index += 2;
        let mut value = Vec::new();
        while index < bytes.len() && bytes[index] != b'"' {
            if bytes[index] == b'\\' && index + 3 < bytes.len() && bytes[index + 1] == b'x' {
                let hex = &line[index + 2..index + 4];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    value.push(byte);
                    index += 4;
                    continue;
                }
            }
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                index += 1;
            }
            value.push(bytes[index]);
            index += 1;
        }
        if index >= bytes.len() {
            return Err(MinfmError::Message(format!(
                "unterminated lsblk value: {line}"
            )));
        }
        index += 1;
        fields.insert(key, String::from_utf8_lossy(&value).into_owned());
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_locked_and_mounted_luks_devices_from_fixture() {
        let fixture = concat!(
            "PATH=\"/dev/sdb2\" TYPE=\"part\" FSTYPE=\"crypto_LUKS\" MOUNTPOINTS=\"\" SIZE=\"1000\" LABEL=\"Vault\" PKNAME=\"/dev/sdb\"\n",
            "PATH=\"/dev/mapper/vault\" TYPE=\"crypt\" FSTYPE=\"ext4\" MOUNTPOINTS=\"/run/media/user/Vault\" SIZE=\"900\" LABEL=\"\" PKNAME=\"/dev/sdb2\"\n",
            "PATH=\"/dev/sdc1\" TYPE=\"part\" FSTYPE=\"crypto_LUKS\" MOUNTPOINTS=\"\" SIZE=\"2000\" LABEL=\"Cold\" PKNAME=\"/dev/sdc\"\n",
        );
        let devices = parse_lsblk(fixture).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].state_text(), "mounted");
        assert_eq!(
            devices[0].mountpoints[0],
            PathBuf::from("/run/media/user/Vault")
        );
        assert_eq!(devices[1].state_text(), "locked");
    }

    #[test]
    fn decodes_lsblk_hex_escapes() {
        let fields = parse_pairs("PATH=\"/dev/a\" LABEL=\"My\\x20Disk\"").unwrap();
        assert_eq!(fields["LABEL"], "My Disk");
    }

    #[test]
    fn marks_devices_backing_system_mounts_as_protected() {
        let fixture = concat!(
            "PATH=\"/dev/sda3\" TYPE=\"part\" FSTYPE=\"crypto_LUKS\" MOUNTPOINTS=\"\" SIZE=\"1000\" LABEL=\"System\" PKNAME=\"/dev/sda\"\n",
            "PATH=\"/dev/mapper/system-root\" TYPE=\"crypt\" FSTYPE=\"ext4\" MOUNTPOINTS=\"/\" SIZE=\"900\" LABEL=\"\" PKNAME=\"/dev/sda3\"\n",
        );
        let devices =
            parse_lsblk_with_protected(fixture, &[PathBuf::from("/dev/mapper/system-root")])
                .unwrap();
        assert!(devices[0].system_protected);
    }

    #[test]
    fn identifies_safe_removable_drive_for_eject() {
        let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" FSTYPE=\"\" MOUNTPOINTS=\"\" SIZE=\"2000\" LABEL=\"\" PKNAME=\"\" RM=\"1\" TRAN=\"usb\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" FSTYPE=\"crypto_LUKS\" MOUNTPOINTS=\"\" SIZE=\"1900\" LABEL=\"Vault\" PKNAME=\"/dev/sdb\" RM=\"1\" TRAN=\"\"\n",
        );
        let devices = parse_lsblk(fixture).unwrap();

        assert_eq!(devices[0].drive, PathBuf::from("/dev/sdb"));
        assert!(devices[0].ejectable);
        assert!(!devices[0].eject_blocked);
    }

    #[test]
    fn blocks_eject_when_another_volume_on_drive_is_active() {
        let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" FSTYPE=\"\" MOUNTPOINTS=\"\" SIZE=\"3000\" LABEL=\"\" PKNAME=\"\" RM=\"1\" TRAN=\"usb\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" FSTYPE=\"crypto_LUKS\" MOUNTPOINTS=\"\" SIZE=\"1900\" LABEL=\"Vault\" PKNAME=\"/dev/sdb\" RM=\"1\" TRAN=\"\"\n",
            "PATH=\"/dev/sdb2\" TYPE=\"part\" FSTYPE=\"ext4\" MOUNTPOINTS=\"/run/media/user/Other\" SIZE=\"900\" LABEL=\"Other\" PKNAME=\"/dev/sdb\" RM=\"1\" TRAN=\"\"\n",
        );
        let devices = parse_lsblk(fixture).unwrap();

        assert!(devices[0].eject_blocked);
    }

    #[test]
    fn secret_debug_output_is_always_redacted() {
        let mut secret = SecretInput::default();
        for character in "not-a-real-passphrase".chars() {
            secret.push(character);
        }
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "SecretInput([REDACTED])");
        assert!(!rendered.contains("not-a-real-passphrase"));
    }

    #[test]
    fn recognizes_common_wrong_passphrase_messages() {
        assert!(incorrect_passphrase(
            b"Error unlocking: The passphrase is incorrect.",
            b""
        ));
        assert!(incorrect_passphrase(
            b"No key available with this passphrase",
            b""
        ));
        assert!(!incorrect_passphrase(
            b"Not authorized to perform operation",
            b""
        ));
    }
}
