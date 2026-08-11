use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use nix::{
    sys::stat::Mode,
    unistd::{mkfifo, Gid, Uid},
};

use crate::{
    block::{self, BlockDevice, BlockInventory},
    error::{MinfmError, Result},
    luks::SecretInput,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionEntry {
    pub device: BlockDevice,
    pub depth: usize,
    pub protected: bool,
    pub mounted_descendants: bool,
}

impl PartitionEntry {
    pub fn state_text(&self) -> &'static str {
        if self.protected {
            "protected"
        } else if self.device.read_only {
            "read only"
        } else if self.device.is_mounted() {
            "mounted"
        } else if self.mounted_descendants {
            "in use"
        } else {
            "available"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionInventory {
    pub entries: Vec<PartitionEntry>,
}

impl PartitionInventory {
    fn from_blocks(inventory: BlockInventory) -> Self {
        let mut children = HashMap::<Option<PathBuf>, Vec<BlockDevice>>::new();
        for device in inventory.devices.iter().cloned() {
            children
                .entry(device.parent.clone())
                .or_default()
                .push(device);
        }
        for devices in children.values_mut() {
            devices.sort_by(|left, right| left.path.cmp(&right.path));
        }

        let mut entries = Vec::new();
        let mut roots = children.remove(&None).unwrap_or_default();
        roots.sort_by(|left, right| left.path.cmp(&right.path));
        for root in roots {
            append_device(&mut entries, &mut children, &inventory, root, 0);
        }

        // Device-mapper output can occasionally reference a parent omitted by
        // lsblk. Keep those records visible rather than silently dropping them.
        let mut orphans = children.into_values().flatten().collect::<Vec<_>>();
        orphans.sort_by(|left, right| left.path.cmp(&right.path));
        for orphan in orphans {
            if !entries.iter().any(|entry| entry.device.path == orphan.path) {
                entries.push(PartitionEntry {
                    protected: inventory.is_protected(&orphan.path),
                    mounted_descendants: inventory.descendants_mounted(&orphan.path),
                    device: orphan,
                    depth: 0,
                });
            }
        }
        Self { entries }
    }
}

fn append_device(
    entries: &mut Vec<PartitionEntry>,
    children: &mut HashMap<Option<PathBuf>, Vec<BlockDevice>>,
    inventory: &BlockInventory,
    device: BlockDevice,
    depth: usize,
) {
    let path = device.path.clone();
    entries.push(PartitionEntry {
        protected: inventory.is_protected(&path),
        mounted_descendants: inventory.descendants_mounted(&path),
        device,
        depth,
    });
    if let Some(mut nested) = children.remove(&Some(path)) {
        nested.sort_by(|left, right| left.path.cmp(&right.path));
        for child in nested {
            append_device(entries, children, inventory, child, depth + 1);
        }
    }
}

pub fn discover() -> Result<PartitionInventory> {
    block::discover().map(PartitionInventory::from_blocks)
}

#[cfg(test)]
pub(crate) fn from_lsblk_fixture(
    text: &str,
    protected_sources: &[PathBuf],
) -> Result<PartitionInventory> {
    block::parse_lsblk(text, protected_sources).map(PartitionInventory::from_blocks)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub path: PathBuf,
    pub major_minor: String,
}

impl DeviceIdentity {
    pub fn from_entry(entry: &PartitionEntry) -> Result<Self> {
        let major_minor = entry.device.major_minor.clone().ok_or_else(|| {
            MinfmError::Message(format!(
                "{} has no stable kernel device identity",
                entry.device.path.display()
            ))
        })?;
        Ok(Self {
            path: entry.device.path.clone(),
            major_minor,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionTable {
    Gpt,
    Msdos,
}

impl PartitionTable {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gpt" => Some(Self::Gpt),
            "msdos" | "mbr" => Some(Self::Msdos),
            _ => None,
        }
    }

    fn parted_name(self) -> &'static str {
        match self {
            Self::Gpt => "gpt",
            Self::Msdos => "msdos",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Gpt => "GPT",
            Self::Msdos => "MBR",
        }
    }

    fn current_matches(self, current: Option<&str>) -> bool {
        matches!(
            (self, current),
            (Self::Gpt, Some("gpt")) | (Self::Msdos, Some("dos" | "msdos" | "mbr"))
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    Ext4,
    Ntfs,
    Fat32,
    Xfs,
    Swap,
    Btrfs,
    F2fs,
    Exfat,
    Udf,
    None,
}

impl Filesystem {
    pub const ALL: [Self; 10] = [
        Self::Ext4,
        Self::Ntfs,
        Self::Fat32,
        Self::Xfs,
        Self::Swap,
        Self::Btrfs,
        Self::F2fs,
        Self::Exfat,
        Self::Udf,
        Self::None,
    ];
    pub const NAMES: &'static str = "ext4, ntfs, fat, xfs, swap, btrfs, f2fs, exfat, udf, none";

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ext4" => Some(Self::Ext4),
            "ntfs" | "ntfs3" => Some(Self::Ntfs),
            "xfs" => Some(Self::Xfs),
            "btrfs" => Some(Self::Btrfs),
            "f2fs" => Some(Self::F2fs),
            "fat32" | "vfat" => Some(Self::Fat32),
            "exfat" => Some(Self::Exfat),
            "swap" => Some(Self::Swap),
            "udf" => Some(Self::Udf),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ext4 => "ext4",
            Self::Ntfs => "NTFS",
            Self::Xfs => "XFS",
            Self::Btrfs => "Btrfs",
            Self::F2fs => "F2FS",
            Self::Fat32 => "FAT32",
            Self::Exfat => "exFAT",
            Self::Swap => "swap",
            Self::Udf => "UDF",
            Self::None => "No filesystem",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ext4 => "Recommended Linux default",
            Self::Ntfs => "For Windows",
            Self::Xfs => "Large files and high-performance storage",
            Self::Btrfs => "Snapshots and advanced Linux features",
            Self::F2fs => "Flash storage on Linux",
            Self::Fat32 => "EFI and broad device compatibility",
            Self::Exfat => "Large files across Linux, macOS, and Windows",
            Self::Swap => "Linux swap space",
            Self::Udf => "Removable media across many systems",
            Self::None => "Leave the partition unformatted",
        }
    }

    fn current_matches(self, current: Option<&str>) -> bool {
        match (self, current) {
            (Self::Fat32, Some("vfat" | "fat" | "fat32")) => true,
            (Self::Swap, Some("swap")) => true,
            (Self::None, None) => true,
            (expected, Some(current)) => Self::parse(current) == Some(expected),
            (_, None) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionAction {
    SmartReport {
        disk: DeviceIdentity,
    },
    SmartTest {
        disk: DeviceIdentity,
        extended: bool,
    },
    DriveSetting {
        disk: DeviceIdentity,
        setting: DriveSetting,
    },
    ChangeLuksPassphrase {
        target: DeviceIdentity,
        old: SecretInput,
        new: SecretInput,
    },
    SetMountOptions {
        target: DeviceIdentity,
        uuid: String,
        mountpoint: PathBuf,
        options: String,
    },
    SetEncryptionOptions {
        target: DeviceIdentity,
        uuid: String,
        name: String,
        options: String,
    },
    Mount {
        target: DeviceIdentity,
    },
    Unmount {
        target: DeviceIdentity,
    },
    CreateTable {
        disk: DeviceIdentity,
        table: PartitionTable,
        overwrite: bool,
    },
    EraseDisk {
        disk: DeviceIdentity,
        overwrite: bool,
    },
    CreatePartition {
        disk: DeviceIdentity,
        start_bytes: u64,
        end_bytes: u64,
    },
    DeletePartition {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
    },
    SetPartitionName {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
        name: String,
    },
    SetPartitionType {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
        type_id: String,
    },
    Format {
        target: DeviceIdentity,
        filesystem: Filesystem,
        label: Option<String>,
    },
    EncryptFormat {
        target: DeviceIdentity,
        filesystem: Filesystem,
        label: Option<String>,
        passphrase: SecretInput,
    },
    CreateEncryptedDisk {
        disk: DeviceIdentity,
        filesystem: Filesystem,
        label: Option<String>,
        passphrase: SecretInput,
    },
    Grow {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
        end_bytes: u64,
        filesystem: Option<Filesystem>,
    },
    Shrink {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
        end_bytes: u64,
        filesystem: Filesystem,
    },
    SetLabel {
        target: DeviceIdentity,
        filesystem: Filesystem,
        label: String,
    },
    CheckFilesystem {
        target: DeviceIdentity,
        filesystem: Filesystem,
    },
    RepairFilesystem {
        target: DeviceIdentity,
        filesystem: Filesystem,
    },
    SetFlag {
        target: DeviceIdentity,
        disk: DeviceIdentity,
        number: u32,
        flag: String,
        enabled: bool,
    },
    BackupTable {
        disk: DeviceIdentity,
        destination: PathBuf,
    },
    CreateImage {
        target: DeviceIdentity,
        destination: PathBuf,
    },
    RestoreImage {
        target: DeviceIdentity,
        source: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveSetting {
    Standby(u8),
    PowerManagement(u8),
    AcousticManagement(u8),
    WriteCache(bool),
}

impl PartitionAction {
    pub fn target(&self) -> &DeviceIdentity {
        match self {
            Self::ChangeLuksPassphrase { target, .. }
            | Self::SetMountOptions { target, .. }
            | Self::SetEncryptionOptions { target, .. }
            | Self::Mount { target }
            | Self::Unmount { target }
            | Self::DeletePartition { target, .. }
            | Self::SetPartitionName { target, .. }
            | Self::SetPartitionType { target, .. }
            | Self::Format { target, .. }
            | Self::EncryptFormat { target, .. }
            | Self::Grow { target, .. }
            | Self::Shrink { target, .. }
            | Self::SetLabel { target, .. }
            | Self::CheckFilesystem { target, .. }
            | Self::RepairFilesystem { target, .. }
            | Self::SetFlag { target, .. } => target,
            Self::CreateImage { target, .. } | Self::RestoreImage { target, .. } => target,
            Self::SmartReport { disk }
            | Self::SmartTest { disk, .. }
            | Self::DriveSetting { disk, .. }
            | Self::CreateTable { disk, .. }
            | Self::EraseDisk { disk, .. }
            | Self::CreatePartition { disk, .. }
            | Self::BackupTable { disk, .. } => disk,
            Self::CreateEncryptedDisk { disk, .. } => disk,
        }
    }

    pub fn is_destructive(&self) -> bool {
        !matches!(
            self,
            Self::Mount { .. }
                | Self::Unmount { .. }
                | Self::SmartReport { .. }
                | Self::SmartTest { .. }
                | Self::CheckFilesystem { .. }
                | Self::BackupTable { .. }
                | Self::CreateImage { .. }
        )
    }

    pub fn erases_data(&self) -> bool {
        matches!(
            self,
            Self::CreateTable { .. }
                | Self::EraseDisk { .. }
                | Self::DeletePartition { .. }
                | Self::Format { .. }
                | Self::EncryptFormat { .. }
                | Self::CreateEncryptedDisk { .. }
                | Self::RestoreImage { .. }
        )
    }

    pub fn warning_text(&self) -> &'static str {
        match self {
            Self::CreateTable { .. } => "This permanently erases the current partition layout.",
            Self::EraseDisk { .. } => {
                "This permanently erases partition and filesystem information on the disk."
            }
            Self::CreatePartition { .. } => {
                "This changes the disk layout. Review the target and size carefully."
            }
            Self::DeletePartition { .. } => {
                "This removes the partition and makes its data inaccessible."
            }
            Self::SetPartitionName { .. } | Self::SetPartitionType { .. } => {
                "This changes partition metadata on the selected disk."
            }
            Self::Format { .. } => "This permanently erases data on the selected device.",
            Self::EncryptFormat { .. } => {
                "This erases the target and creates LUKS2. Losing the passphrase makes its data inaccessible."
            }
            Self::CreateEncryptedDisk { .. } => {
                "This erases the disk, creates GPT with one LUKS2 partition, and formats its encrypted contents."
            }
            Self::Grow { .. }
            | Self::Shrink { .. }
            | Self::SetLabel { .. }
            | Self::SetFlag { .. } => "This changes data stored on the selected device.",
            Self::BackupTable { .. } => "This reads the partition table without changing it.",
            Self::CreateImage { .. } => "This reads the device without changing it.",
            Self::RestoreImage { .. } => "This replaces all data on the selected device.",
            Self::RepairFilesystem { .. } => {
                "Repair can change filesystem data. Keep a backup when possible."
            }
            Self::ChangeLuksPassphrase { .. } => "The old passphrase stops unlocking this volume.",
            Self::SetMountOptions { .. } => "This updates the persistent system mount configuration.",
            Self::SetEncryptionOptions { .. } => "This updates the persistent encrypted-volume configuration.",
            Self::SmartReport { .. } => "This reads drive health data without changing it.",
            Self::SmartTest { .. } => "This starts a drive self-test without erasing data.",
            Self::DriveSetting { .. } => "This changes a hardware setting until the drive resets.",
            Self::Mount { .. } | Self::Unmount { .. } | Self::CheckFilesystem { .. } => {
                "Proceed with this operation?"
            }
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::ChangeLuksPassphrase { .. } => "Change LUKS passphrase",
            Self::SetMountOptions { .. } => "Change mount options",
            Self::SetEncryptionOptions { .. } => "Change encryption options",
            Self::SmartReport { .. } => "SMART report",
            Self::SmartTest {
                extended: false, ..
            } => "Start short SMART test",
            Self::SmartTest { extended: true, .. } => "Start extended SMART test",
            Self::DriveSetting { .. } => "Change drive setting",
            Self::Mount { .. } => "Mount filesystem",
            Self::Unmount { .. } => "Unmount filesystem",
            Self::CreateTable { .. } => "Create table",
            Self::EraseDisk { .. } => "Leave disk empty",
            Self::CreatePartition { .. } => "Create partition",
            Self::DeletePartition { .. } => "Delete partition",
            Self::SetPartitionName { .. } => "Rename partition",
            Self::SetPartitionType { .. } => "Change partition type",
            Self::Format { .. } => "Format",
            Self::EncryptFormat { .. } => "Encrypt and format",
            Self::CreateEncryptedDisk { .. } => "Create encrypted disk",
            Self::Grow { .. } => "Grow partition",
            Self::Shrink { .. } => "Shrink partition",
            Self::SetLabel { .. } => "Change filesystem label",
            Self::CheckFilesystem { .. } => "Check filesystem",
            Self::RepairFilesystem { .. } => "Repair filesystem",
            Self::SetFlag { .. } => "Change partition flag",
            Self::BackupTable { .. } => "Back up partition table",
            Self::CreateImage { .. } => "Create image",
            Self::RestoreImage { .. } => "Restore image",
        }
    }

    pub fn confirmation_text(&self) -> String {
        let path = self.target().path.display();
        match self {
            Self::ChangeLuksPassphrase { .. } => format!("Replace the LUKS passphrase for {path}."),
            Self::SetMountOptions { mountpoint, options, .. } => format!("Mount {path} at {} with {options} after startup.", mountpoint.display()),
            Self::SetEncryptionOptions { name, options, .. } => format!("Configure {path} as {name} with {options}."),
            Self::SmartReport { .. } => format!("Read SMART health data from {path}."),
            Self::SmartTest { extended, .. } => format!(
                "Start a {} SMART self-test on {path}.",
                if *extended { "extended" } else { "short" }
            ),
            Self::DriveSetting { setting, .. } => format!("Apply {setting:?} to {path}."),
            Self::Mount { .. } => format!("Mount {path} through udisksctl."),
            Self::Unmount { .. } => format!("Unmount {path} through udisksctl."),
            Self::CreateTable { table, .. } => format!(
                "Create a {} partition table on {path}. Existing partition information will be erased.",
                table.display_name()
            ),
            Self::EraseDisk { .. } => format!(
                "Erase partition and filesystem information from {path} and leave it without a partition table."
            ),
            Self::CreatePartition {
                start_bytes,
                end_bytes,
                ..
            } => format!(
                "Create a {} partition on {path} using available disk space.",
                format_bytes(end_bytes.saturating_sub(*start_bytes))
            ),
            Self::DeletePartition { number, .. } => {
                format!("Delete partition {number} ({path}). Its data will become inaccessible.")
            }
            Self::SetPartitionName { number, name, .. } => {
                format!("Set partition {number} on {path} to the GPT name {name:?}.")
            }
            Self::SetPartitionType {
                number, type_id, ..
            } => format!("Set partition {number} on {path} to type {type_id:?}."),
            Self::Format {
                filesystem, label, ..
            } => format!(
                "Erase {path} and format it as {}{}.",
                filesystem.name(),
                label
                    .as_ref()
                    .map(|label| format!(" labeled {label:?}"))
                    .unwrap_or_default()
            ),
            Self::EncryptFormat { filesystem, .. } => format!(
                "Erase {path}, create a LUKS2 encrypted container, and format its unlocked contents as {}.",
                filesystem.name()
            ),
            Self::CreateEncryptedDisk { filesystem, .. } => format!(
                "Erase {path}, create GPT with one LUKS2 encrypted partition, and format its unlocked contents as {}.",
                filesystem.name()
            ),
            Self::Grow { end_bytes, .. } => format!(
                "Grow {path} to end at {}. Shrinking is never performed.",
                format_bytes(*end_bytes)
            ),
            Self::Shrink { end_bytes, .. } => format!(
                "Shrink {path} to end at {}. The filesystem is reduced before its partition.",
                format_bytes(*end_bytes)
            ),
            Self::SetLabel { label, .. } => {
                format!("Change the filesystem label on {path} to {label:?}.")
            }
            Self::CheckFilesystem { .. } => {
                format!("Run a read-only filesystem check on {path}.")
            }
            Self::RepairFilesystem { .. } => format!("Repair the filesystem on {path}."),
            Self::SetFlag { flag, enabled, .. } => format!(
                "Turn partition flag {flag:?} {} for {path}.",
                if *enabled { "on" } else { "off" }
            ),
            Self::BackupTable { destination, .. } => format!(
                "Save a restorable text backup of the partition table on {path} to {}.",
                destination.display()
            ),
            Self::CreateImage { destination, .. } => {
                format!("Save an image of {path} to {}.", destination.display())
            }
            Self::RestoreImage { source, .. } => {
                format!("Replace {path} with image {}.", source.display())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: OsString,
    arguments: Vec<OsString>,
    elevated: bool,
    accepted_codes: Vec<i32>,
}

impl CommandSpec {
    fn elevated(program: &str, arguments: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            elevated: true,
            accepted_codes: vec![0],
        }
    }

    fn user(program: &str, arguments: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            program: program.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            elevated: false,
            accepted_codes: vec![0],
        }
    }
}

pub fn authentication_required(action: &PartitionAction) -> bool {
    !Uid::effective().is_root()
        && !matches!(
            action,
            PartitionAction::Mount { .. } | PartitionAction::Unmount { .. }
        )
}

pub fn helper_available(program: &str) -> bool {
    trusted_program(&OsString::from(program)).is_ok()
}

pub fn execute(
    action: &PartitionAction,
    administrator_password: Option<&[u8]>,
    mut report_phase: impl FnMut(&'static str),
) -> Result<String> {
    report_phase("Revalidating device identity and safety");
    let mut inventory = discover()?;
    validate_action(action, &inventory)?;
    let commands = command_plan(action, &inventory)?;
    let custom_elevated = matches!(
        action,
        PartitionAction::EncryptFormat { .. }
            | PartitionAction::CreateEncryptedDisk { .. }
            | PartitionAction::ChangeLuksPassphrase { .. }
            | PartitionAction::SetMountOptions { .. }
            | PartitionAction::SetEncryptionOptions { .. }
    );
    let use_sudo = !Uid::effective().is_root()
        && (custom_elevated || commands.iter().any(|command| command.elevated));
    let _sudo_session = if use_sudo {
        report_phase("Authenticating administrator");
        let program = authenticate_sudo(administrator_password.ok_or_else(|| {
            MinfmError::Message("administrator authentication is required".into())
        })?)?;
        report_phase("Revalidating device safety after authentication");
        inventory = discover()?;
        validate_action(action, &inventory)?;
        Some(SudoSession { program })
    } else {
        None
    };
    if matches!(
        action,
        PartitionAction::SetMountOptions { .. } | PartitionAction::SetEncryptionOptions { .. }
    ) {
        report_phase("Updating persistent device options");
        execute_persistent_options(action, use_sudo, administrator_password)?;
    } else if matches!(action, PartitionAction::ChangeLuksPassphrase { .. }) {
        report_phase("Changing LUKS passphrase");
        execute_luks_passphrase_change(action, use_sudo)?;
    } else if matches!(
        action,
        PartitionAction::EncryptFormat { .. } | PartitionAction::CreateEncryptedDisk { .. }
    ) {
        execute_encrypted_format(
            action,
            &inventory,
            use_sudo,
            administrator_password,
            &mut report_phase,
        )?;
    } else if matches!(action, PartitionAction::SmartReport { .. }) {
        report_phase("Reading SMART health data");
        let output = run_command(&commands[0], use_sudo, administrator_password)?;
        let report = summarize_smart_report(&String::from_utf8_lossy(&output));
        return Ok(report);
    } else {
        for command in commands {
            report_phase(command_phase(&command));
            let output = run_command(&command, use_sudo, administrator_password)?;
            if let PartitionAction::BackupTable { destination, .. } = action {
                write_new_backup(destination, &output)?;
            }
        }
    }
    if matches!(action, PartitionAction::BackupTable { .. }) {
        return Ok(format!("{} completed", action.title()));
    }
    report_phase("Waiting for the kernel device view");
    settle_devices();
    report_phase("Confirming final device state");
    let inventory = discover()?;
    if matches!(action, PartitionAction::SetFlag { .. }) {
        verify_partition_flag(action, use_sudo, administrator_password)?;
    }
    verify_final_state(action, &inventory)?;
    Ok(match action {
        PartitionAction::CreateTable { table, .. } => format!(
            "{} table created; select the disk and choose Create partition",
            table.display_name()
        ),
        _ => format!("{} completed", action.title()),
    })
}

fn summarize_smart_report(report: &str) -> String {
    const FIELDS: [&str; 13] = [
        "Device Model",
        "Model Number",
        "Serial Number",
        "Firmware Version",
        "User Capacity",
        "SMART overall-health",
        "SMART Health Status",
        "Critical Warning",
        "Temperature",
        "Available Spare",
        "Percentage Used",
        "Power_On_Hours",
        "Self-test execution status",
    ];
    let mut lines = report
        .lines()
        .map(str::trim)
        .filter(|line| FIELDS.iter().any(|field| line.contains(field)))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.dedup();
    lines.truncate(18);
    if lines.is_empty() {
        "SMART data is available, but no standard summary fields were reported.".into()
    } else {
        lines.join("\n")
    }
}

struct PrivatePipeDir(PathBuf);

impl Drop for PrivatePipeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn execute_luks_passphrase_change(action: &PartitionAction, use_sudo: bool) -> Result<()> {
    let PartitionAction::ChangeLuksPassphrase { target, old, new } = action else {
        return Err(MinfmError::Message(
            "expected a LUKS passphrase action".into(),
        ));
    };
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MinfmError::Message("system clock is unavailable".into()))?
        .as_nanos();
    let directory = env::temp_dir().join(format!("minfm-key-{}-{unique}", std::process::id()));
    fs::create_dir(&directory)
        .map_err(|error| crate::error::io_error("could not create a private key pipe", error))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| crate::error::io_error("could not secure the key pipe", error))?;
    let cleanup = PrivatePipeDir(directory.clone());
    let fifo = directory.join("new-key");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR)
        .map_err(|error| MinfmError::Message(format!("could not create key pipe: {error}")))?;
    let keeper = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .map_err(|error| crate::error::io_error("could not open key pipe", error))?;
    let secret = new.clone();
    let writer_path = fifo.clone();
    let writer = thread::spawn(move || -> std::io::Result<()> {
        OpenOptions::new()
            .write(true)
            .open(writer_path)?
            .write_all(secret.expose())
    });
    let result = run_command_with_secret(
        "cryptsetup",
        [
            OsString::from("luksChangeKey"),
            OsString::from("--key-file"),
            OsString::from("-"),
            OsString::from("--new-keyfile"),
            fifo.into_os_string(),
            OsString::from("--new-keyfile-size"),
            OsString::from(new.expose().len().to_string()),
            target.path.as_os_str().to_owned(),
        ],
        old.expose(),
        use_sudo,
    );
    drop(keeper);
    writer
        .join()
        .map_err(|_| MinfmError::Message("key pipe writer stopped unexpectedly".into()))?
        .map_err(|error| crate::error::io_error("could not send the new key", error))?;
    drop(cleanup);
    result
}

fn validate_option_field(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_,=.-".contains(character))
    {
        return Err(MinfmError::Message(
            "options may contain letters, numbers, commas, equals, dots, underscores, and dashes"
                .into(),
        ));
    }
    Ok(())
}

fn replace_config_entry(contents: &str, uuid: &str, replacement: &str) -> String {
    let marker = format!("UUID={uuid}");
    let mut replaced = false;
    let mut output = String::new();
    for line in contents.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let matches =
            !line.trim_start().starts_with('#') && fields.iter().any(|field| *field == marker);
        if matches {
            if !replaced {
                output.push_str(replacement);
                output.push('\n');
                replaced = true;
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        if !output.is_empty() && !output.ends_with("\n\n") {
            output.push('\n');
        }
        output.push_str(replacement);
        output.push('\n');
    }
    output
}

pub fn current_mount_options(uuid: &str) -> Option<(PathBuf, String)> {
    let contents = fs::read_to_string("/etc/fstab").ok()?;
    let marker = format!("UUID={uuid}");
    contents.lines().find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        (!line.trim_start().starts_with('#')
            && fields.first() == Some(&marker.as_str())
            && fields.len() >= 4)
            .then(|| (PathBuf::from(fields[1]), fields[3].to_owned()))
    })
}

pub fn current_encryption_options(uuid: &str) -> Option<(String, String)> {
    let contents = fs::read_to_string("/etc/crypttab").ok()?;
    let marker = format!("UUID={uuid}");
    contents.lines().find_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        (!line.trim_start().starts_with('#')
            && fields.get(1) == Some(&marker.as_str())
            && fields.len() >= 4)
            .then(|| (fields[0].to_owned(), fields[3].to_owned()))
    })
}

fn execute_persistent_options(
    action: &PartitionAction,
    use_sudo: bool,
    administrator_password: Option<&[u8]>,
) -> Result<()> {
    let (system_path, uuid, replacement, mountpoint) = match action {
        PartitionAction::SetMountOptions {
            uuid,
            mountpoint,
            options,
            ..
        } => (
            Path::new("/etc/fstab"),
            uuid,
            format!(
                "UUID={uuid}\t{}\tauto\t{options}\t0\t0",
                mountpoint.display()
            ),
            Some(mountpoint.as_path()),
        ),
        PartitionAction::SetEncryptionOptions {
            uuid,
            name,
            options,
            ..
        } => (
            Path::new("/etc/crypttab"),
            uuid,
            format!("{name}\tUUID={uuid}\tnone\t{options}"),
            None,
        ),
        _ => {
            return Err(MinfmError::Message(
                "expected persistent device options".into(),
            ))
        }
    };
    let current = match fs::read_to_string(system_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(crate::error::io_error(
                "could not read system configuration",
                error,
            ))
        }
    };
    let updated = replace_config_entry(&current, uuid, &replacement);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MinfmError::Message("system clock is unavailable".into()))?
        .as_nanos();
    let directory = env::temp_dir().join(format!("minfm-config-{}-{unique}", std::process::id()));
    fs::create_dir(&directory)
        .map_err(|error| crate::error::io_error("could not stage device options", error))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| crate::error::io_error("could not secure staged options", error))?;
    let cleanup = PrivatePipeDir(directory.clone());
    let staged = directory.join("configuration");
    fs::write(&staged, updated)
        .map_err(|error| crate::error::io_error("could not stage device options", error))?;
    if let Some(mountpoint) = mountpoint {
        run_command(
            &CommandSpec::elevated(
                "mkdir",
                [
                    OsString::from("--parents"),
                    mountpoint.as_os_str().to_owned(),
                ],
            ),
            use_sudo,
            administrator_password,
        )?;
    }
    let pending = system_path.with_extension("minfm-new");
    run_command(
        &CommandSpec::elevated(
            "install",
            [
                OsString::from("--mode=0644"),
                staged.into_os_string(),
                pending.as_os_str().to_owned(),
            ],
        ),
        use_sudo,
        administrator_password,
    )?;
    run_command(
        &CommandSpec::elevated(
            "mv",
            [
                OsString::from("--force"),
                pending.into_os_string(),
                system_path.as_os_str().to_owned(),
            ],
        ),
        use_sudo,
        administrator_password,
    )?;
    drop(cleanup);
    Ok(())
}

fn execute_encrypted_format(
    action: &PartitionAction,
    inventory: &PartitionInventory,
    use_sudo: bool,
    administrator_password: Option<&[u8]>,
    report_phase: &mut impl FnMut(&'static str),
) -> Result<()> {
    let _ = trusted_program(&OsString::from("cryptsetup"))?;
    let expected_filesystem = match action {
        PartitionAction::EncryptFormat { filesystem, .. }
        | PartitionAction::CreateEncryptedDisk { filesystem, .. } => *filesystem,
        _ => {
            return Err(MinfmError::Message(
                "expected an encrypted format action".into(),
            ))
        }
    };
    let formatter = format_command(
        OsString::from("/dev/mapper/minfm-preflight"),
        expected_filesystem,
        None,
    );
    let _ = trusted_program(&formatter.program)?;
    let mapping_name = format!("minfm-{}", action.target().major_minor.replace(':', "-"));
    if Path::new("/dev/mapper").join(&mapping_name).exists() {
        return Err(MinfmError::Message(format!(
            "encrypted mapping {mapping_name} already exists; close it before retrying"
        )));
    }
    let (filesystem, label, passphrase, target_path) = match action {
        PartitionAction::EncryptFormat {
            target,
            filesystem,
            label,
            passphrase,
        } => (
            *filesystem,
            label.as_deref(),
            passphrase,
            target.path.clone(),
        ),
        PartitionAction::CreateEncryptedDisk {
            disk,
            filesystem,
            label,
            passphrase,
        } => {
            report_phase("Creating GPT partition layout");
            let mut commands = wipe_disk_commands(disk, inventory);
            commands.extend([
                CommandSpec::elevated(
                    "parted",
                    [
                        OsString::from("--script"),
                        disk.path.as_os_str().to_owned(),
                        OsString::from("mklabel"),
                        OsString::from("gpt"),
                    ],
                ),
                CommandSpec::elevated(
                    "parted",
                    [
                        OsString::from("--script"),
                        OsString::from("--align"),
                        OsString::from("optimal"),
                        disk.path.as_os_str().to_owned(),
                        OsString::from("mkpart"),
                        OsString::from("primary"),
                        OsString::from("1MiB"),
                        OsString::from("100%"),
                    ],
                ),
                reread_partition_table_command(disk),
            ]);
            for command in commands {
                let _ = run_command(&command, use_sudo, administrator_password)?;
            }
            settle_devices();
            let refreshed = discover()?;
            let partition = refreshed
                .entries
                .iter()
                .find(|entry| {
                    entry.device.kind == "part"
                        && entry.device.parent.as_ref() == Some(&disk.path)
                        && !entry.protected
                })
                .ok_or_else(|| {
                    MinfmError::Message(
                        "GPT was created, but its encrypted partition was not discovered".into(),
                    )
                })?;
            (
                *filesystem,
                label.as_deref(),
                passphrase,
                partition.device.path.clone(),
            )
        }
        _ => {
            return Err(MinfmError::Message(
                "expected an encrypted format action".into(),
            ))
        }
    };

    report_phase("Removing old filesystem signatures");
    let _ = run_command(
        &CommandSpec::elevated(
            "wipefs",
            [
                OsString::from("--all"),
                OsString::from("--force"),
                target_path.as_os_str().to_owned(),
            ],
        ),
        use_sudo,
        administrator_password,
    )?;
    report_phase("Creating LUKS2 encrypted container");
    run_command_with_secret(
        "cryptsetup",
        [
            OsString::from("luksFormat"),
            OsString::from("--type"),
            OsString::from("luks2"),
            OsString::from("--batch-mode"),
            OsString::from("--key-file"),
            OsString::from("-"),
            target_path.as_os_str().to_owned(),
        ],
        passphrase.expose(),
        use_sudo,
    )?;
    report_phase("Opening encrypted container");
    run_command_with_secret(
        "cryptsetup",
        [
            OsString::from("open"),
            OsString::from("--key-file"),
            OsString::from("-"),
            target_path.as_os_str().to_owned(),
            OsString::from(&mapping_name),
        ],
        passphrase.expose(),
        use_sudo,
    )?;
    let mapping = Path::new("/dev/mapper").join(&mapping_name);
    report_phase("Creating filesystem inside encryption");
    let format_result = run_command(
        &format_command(mapping.into_os_string(), filesystem, label),
        use_sudo,
        administrator_password,
    );
    report_phase("Closing encrypted container");
    let close_result = run_command(
        &CommandSpec::elevated(
            "cryptsetup",
            [OsString::from("close"), OsString::from(mapping_name)],
        ),
        use_sudo,
        administrator_password,
    );
    format_result?;
    close_result?;
    Ok(())
}

fn command_phase(command: &CommandSpec) -> &'static str {
    match command.program.to_string_lossy().as_ref() {
        "udisksctl"
            if command
                .arguments
                .first()
                .is_some_and(|argument| argument == "mount") =>
        {
            "Mounting filesystem"
        }
        "udisksctl" => "Unmounting filesystem",
        "parted" => "Updating partition table",
        "blockdev" => "Reloading kernel partition map",
        "mkfs.ext4" | "mkfs.xfs" | "mkfs.btrfs" | "mkfs.fat" | "mkfs.exfat" | "mkswap" => {
            "Creating filesystem"
        }
        "e2fsck" | "xfs_repair" | "fsck.fat" | "fsck.exfat" => "Checking filesystem",
        "resize2fs" => "Growing filesystem",
        "e2label" | "xfs_admin" | "btrfs" | "fatlabel" | "exfatlabel" | "swaplabel" => {
            "Updating filesystem metadata"
        }
        "wipefs" => "Removing storage signatures",
        "sfdisk" => "Reading partition table",
        "smartctl" => "Running SMART operation",
        "hdparm" => "Updating drive settings",
        _ => "Running partition operation",
    }
}

pub fn validate_snapshot(action: &PartitionAction, entries: &[PartitionEntry]) -> Result<()> {
    validate_action(
        action,
        &PartitionInventory {
            entries: entries.to_vec(),
        },
    )
}

fn validate_action(action: &PartitionAction, inventory: &PartitionInventory) -> Result<()> {
    let target = matching_entry(inventory, action.target())?;
    let read_only_operation = matches!(
        action,
        PartitionAction::CheckFilesystem { .. }
            | PartitionAction::BackupTable { .. }
            | PartitionAction::CreateImage { .. }
            | PartitionAction::SmartReport { .. }
            | PartitionAction::SmartTest { .. }
    );
    if target.protected && !read_only_operation {
        return Err(MinfmError::Message(format!(
            "{} belongs to protected system storage",
            target.device.path.display()
        )));
    }
    if target.device.read_only && !read_only_operation {
        return Err(MinfmError::Message(format!(
            "{} is read only",
            target.device.path.display()
        )));
    }

    match action {
        PartitionAction::ChangeLuksPassphrase { old, new, .. } => {
            require_inactive(target)?;
            if target.device.filesystem.as_deref() != Some("crypto_LUKS") {
                return Err(MinfmError::Message("select a locked LUKS volume".into()));
            }
            if old.is_empty() {
                return Err(MinfmError::Message("enter the current passphrase".into()));
            }
            if new.character_count() < 8 {
                return Err(MinfmError::Message(
                    "the new passphrase must contain at least 8 characters".into(),
                ));
            }
        }
        PartitionAction::SetMountOptions {
            uuid,
            mountpoint,
            options,
            ..
        } => {
            if uuid.is_empty()
                || !mountpoint.is_absolute()
                || mountpoint.components().any(|component| {
                    !matches!(component, Component::RootDir | Component::Normal(_))
                })
            {
                return Err(MinfmError::Message(
                    "a UUID and absolute mount point are required".into(),
                ));
            }
            validate_option_field(options)?;
        }
        PartitionAction::SetEncryptionOptions {
            uuid,
            name,
            options,
            ..
        } => {
            if uuid.is_empty()
                || name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            {
                return Err(MinfmError::Message(
                    "enter a safe mapping name and UUID".into(),
                ));
            }
            validate_option_field(options)?;
            if target.device.filesystem.as_deref() != Some("crypto_LUKS") {
                return Err(MinfmError::Message("select a LUKS volume".into()));
            }
        }
        PartitionAction::SmartReport { .. } | PartitionAction::SmartTest { .. } => {
            require_disk(target)?;
        }
        PartitionAction::DriveSetting { setting, .. } => {
            require_disk(target)?;
            match setting {
                DriveSetting::Standby(_) | DriveSetting::WriteCache(_) => {}
                DriveSetting::PowerManagement(value) if *value >= 1 => {}
                DriveSetting::AcousticManagement(value) if (128..=254).contains(value) => {}
                DriveSetting::PowerManagement(_) => {
                    return Err(MinfmError::Message("APM must be between 1 and 255".into()))
                }
                DriveSetting::AcousticManagement(_) => {
                    return Err(MinfmError::Message(
                        "AAM must be between 128 and 254".into(),
                    ))
                }
            }
        }
        PartitionAction::Mount { .. } => {
            if target.device.is_mounted() {
                return Err(MinfmError::Message(
                    "the filesystem is already mounted".into(),
                ));
            }
            if target.device.filesystem.is_none() {
                return Err(MinfmError::Message(
                    "the selected device has no recognized filesystem".into(),
                ));
            }
        }
        PartitionAction::Unmount { .. } => {
            if !target.device.is_mounted() {
                return Err(MinfmError::Message("the filesystem is not mounted".into()));
            }
        }
        PartitionAction::CreateTable { .. } | PartitionAction::EraseDisk { .. } => {
            require_disk(target)?;
            require_inactive(target)?;
        }
        PartitionAction::CreatePartition {
            start_bytes,
            end_bytes,
            ..
        } => {
            require_disk(target)?;
            require_inactive(target)?;
            validate_new_extent(target, inventory, *start_bytes, *end_bytes)?;
        }
        PartitionAction::DeletePartition { disk, number, .. }
        | PartitionAction::SetFlag { disk, number, .. }
        | PartitionAction::SetPartitionName { disk, number, .. }
        | PartitionAction::SetPartitionType { disk, number, .. } => {
            require_partition(target, *number)?;
            require_inactive(target)?;
            let disk = matching_entry(inventory, disk)?;
            require_parent_disk(target, disk)?;
            require_safe_disk_for_table_change(disk)?;
            if let PartitionAction::SetFlag { flag, .. } = action {
                validate_flag(flag)?;
            }
            if let PartitionAction::SetPartitionName { name, .. } = action {
                validate_partition_name(name)?;
                if disk.device.table_type.as_deref() != Some("gpt") {
                    return Err(MinfmError::Message(
                        "partition names require a GPT partition table".into(),
                    ));
                }
            }
            if let PartitionAction::SetPartitionType { type_id, .. } = action {
                validate_partition_type(type_id)?;
            }
        }
        PartitionAction::Format { label, .. } => {
            require_inactive(target)?;
            validate_label(label.as_deref())?;
        }
        PartitionAction::EncryptFormat { passphrase, .. } => {
            require_inactive(target)?;
            if passphrase.character_count() < 8 {
                return Err(MinfmError::Message(
                    "the encryption passphrase must contain at least 8 characters".into(),
                ));
            }
        }
        PartitionAction::CreateEncryptedDisk { passphrase, .. } => {
            require_disk(target)?;
            require_inactive(target)?;
            require_no_mapped_descendants(target, inventory)?;
            if passphrase.character_count() < 8 {
                return Err(MinfmError::Message(
                    "the encryption passphrase must contain at least 8 characters".into(),
                ));
            }
        }
        PartitionAction::Grow {
            disk,
            number,
            end_bytes,
            filesystem,
            ..
        } => {
            require_partition(target, *number)?;
            require_inactive(target)?;
            let disk = matching_entry(inventory, disk)?;
            require_parent_disk(target, disk)?;
            require_safe_disk_for_table_change(disk)?;
            let current_end = target.device.end_bytes().ok_or_else(|| {
                MinfmError::Message("the current partition boundary is unavailable".into())
            })?;
            let current_start = target.device.start_bytes().ok_or_else(|| {
                MinfmError::Message("the current partition boundary is unavailable".into())
            })?;
            if *end_bytes <= current_end || *end_bytes > disk.device.size {
                return Err(MinfmError::Message(
                    "the new boundary must be larger than the partition and inside its disk".into(),
                ));
            }
            let overlaps = inventory.entries.iter().any(|entry| {
                entry.device.path != target.device.path
                    && entry.device.parent.as_ref() == Some(&disk.device.path)
                    && entry
                        .device
                        .start_bytes()
                        .zip(entry.device.end_bytes())
                        .is_some_and(|(start, end)| current_start < end && *end_bytes > start)
            });
            if overlaps {
                return Err(MinfmError::Message(
                    "the requested growth would overlap another partition".into(),
                ));
            }
            if filesystem.is_some_and(|filesystem| filesystem != Filesystem::Ext4) {
                return Err(MinfmError::Message(
                    "automatic filesystem growth currently supports ext4 only".into(),
                ));
            }
        }
        PartitionAction::Shrink {
            disk,
            number,
            end_bytes,
            filesystem,
            ..
        } => {
            require_partition(target, *number)?;
            require_inactive(target)?;
            require_filesystem(target, *filesystem)?;
            if *filesystem != Filesystem::Ext4 {
                return Err(MinfmError::Message(
                    "safe shrinking currently supports ext4 only".into(),
                ));
            }
            let disk = matching_entry(inventory, disk)?;
            require_parent_disk(target, disk)?;
            require_safe_disk_for_table_change(disk)?;
            let start = target.device.start_bytes().ok_or_else(|| {
                MinfmError::Message("the current partition boundary is unavailable".into())
            })?;
            let current_end = target.device.end_bytes().ok_or_else(|| {
                MinfmError::Message("the current partition boundary is unavailable".into())
            })?;
            if *end_bytes >= current_end || end_bytes.saturating_sub(start) < 16 * 1024 * 1024 {
                return Err(MinfmError::Message(
                    "the new ext4 partition size must be smaller and at least 16 MiB".into(),
                ));
            }
            let sector = disk.device.logical_sector_size.max(1);
            if !end_bytes.is_multiple_of(sector) {
                return Err(MinfmError::Message(
                    "the new partition boundary is not sector aligned".into(),
                ));
            }
        }
        PartitionAction::SetLabel {
            filesystem, label, ..
        } => {
            require_inactive(target)?;
            require_filesystem(target, *filesystem)?;
            validate_label(Some(label))?;
        }
        PartitionAction::CheckFilesystem { filesystem, .. } => {
            require_inactive(target)?;
            require_filesystem(target, *filesystem)?;
            if *filesystem == Filesystem::Swap {
                return Err(MinfmError::Message(
                    "swap does not support a read-only filesystem check".into(),
                ));
            }
        }
        PartitionAction::RepairFilesystem { filesystem, .. } => {
            require_inactive(target)?;
            require_filesystem(target, *filesystem)?;
            if matches!(filesystem, Filesystem::Swap | Filesystem::None) {
                return Err(MinfmError::Message(
                    "this filesystem has no repair operation".into(),
                ));
            }
        }
        PartitionAction::BackupTable { destination, .. } => {
            require_disk(target)?;
            if target.device.table_type.is_none() {
                return Err(MinfmError::Message(
                    "the selected disk has no partition table to back up".into(),
                ));
            }
            validate_backup_destination(destination)?;
        }
        PartitionAction::CreateImage { destination, .. } => {
            validate_new_image_destination(destination)?;
        }
        PartitionAction::RestoreImage { source, .. } => {
            require_inactive(target)?;
            let metadata = fs::metadata(source).map_err(|error| {
                crate::error::io_error("could not inspect the disk image", error)
            })?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err(MinfmError::Message(
                    "the selected disk image is empty or not a regular file".into(),
                ));
            }
            if metadata.len() > target.device.size {
                return Err(MinfmError::Message(
                    "the disk image is larger than the selected device".into(),
                ));
            }
        }
    }
    Ok(())
}

fn matching_entry<'a>(
    inventory: &'a PartitionInventory,
    identity: &DeviceIdentity,
) -> Result<&'a PartitionEntry> {
    let entry = inventory
        .entries
        .iter()
        .find(|entry| entry.device.path == identity.path)
        .ok_or_else(|| {
            MinfmError::Message(format!("{} is no longer present", identity.path.display()))
        })?;
    if entry.device.major_minor.as_deref() != Some(identity.major_minor.as_str()) {
        return Err(MinfmError::Message(format!(
            "{} now refers to a different kernel device; the operation was cancelled",
            identity.path.display()
        )));
    }
    Ok(entry)
}

fn require_disk(entry: &PartitionEntry) -> Result<()> {
    if !entry.device.is_disk() {
        return Err(MinfmError::Message(
            "the selected target is not a disk".into(),
        ));
    }
    Ok(())
}

fn require_partition(entry: &PartitionEntry, number: u32) -> Result<()> {
    if entry.device.kind != "part" || entry.device.partition_number != Some(number) {
        return Err(MinfmError::Message(
            "the selected partition number changed".into(),
        ));
    }
    Ok(())
}

fn require_parent_disk(partition: &PartitionEntry, disk: &PartitionEntry) -> Result<()> {
    require_disk(disk)?;
    if partition.device.parent.as_ref() != Some(&disk.device.path) {
        return Err(MinfmError::Message(
            "the partition no longer belongs to the expected disk".into(),
        ));
    }
    Ok(())
}

fn require_safe_disk_for_table_change(disk: &PartitionEntry) -> Result<()> {
    if disk.protected || disk.device.read_only || disk.mounted_descendants {
        return Err(MinfmError::Message(
            "the parent disk is protected, read only, or contains mounted storage".into(),
        ));
    }
    Ok(())
}

fn require_inactive(entry: &PartitionEntry) -> Result<()> {
    if entry.device.is_mounted() || entry.mounted_descendants {
        return Err(MinfmError::Message(format!(
            "{} or one of its children is mounted",
            entry.device.path.display()
        )));
    }
    Ok(())
}

fn require_no_mapped_descendants(
    disk: &PartitionEntry,
    inventory: &PartitionInventory,
) -> Result<()> {
    let mapped = inventory.entries.iter().find(|candidate| {
        candidate.device.kind != "part"
            && candidate.device.path != disk.device.path
            && is_descendant_of(&candidate.device, &disk.device.path, inventory)
    });
    if let Some(mapped) = mapped {
        return Err(MinfmError::Message(format!(
            "{} has an active {} mapping at {}; deactivate it before erasing the disk",
            disk.device.path.display(),
            mapped.device.kind,
            mapped.device.path.display()
        )));
    }
    Ok(())
}

fn is_descendant_of(
    candidate: &BlockDevice,
    ancestor: &Path,
    inventory: &PartitionInventory,
) -> bool {
    let mut parent = candidate.parent.as_ref();
    for _ in 0..inventory.entries.len() {
        let Some(path) = parent else {
            return false;
        };
        if path == ancestor {
            return true;
        }
        parent = inventory
            .entries
            .iter()
            .find(|entry| entry.device.path == *path)
            .and_then(|entry| entry.device.parent.as_ref());
    }
    false
}

fn require_filesystem(entry: &PartitionEntry, filesystem: Filesystem) -> Result<()> {
    if !filesystem.current_matches(entry.device.filesystem.as_deref()) {
        return Err(MinfmError::Message(
            "the filesystem type changed before the operation started".into(),
        ));
    }
    Ok(())
}

fn validate_new_extent(
    disk: &PartitionEntry,
    inventory: &PartitionInventory,
    start: u64,
    end: u64,
) -> Result<()> {
    if start < 1024 * 1024 || start >= end || end > disk.device.size {
        return Err(MinfmError::Message(
            "the requested partition boundary is outside the usable disk range".into(),
        ));
    }
    let sector = disk.device.logical_sector_size.max(1);
    if !start.is_multiple_of(sector) || !end.is_multiple_of(sector) {
        return Err(MinfmError::Message(format!(
            "partition boundaries must align to the disk's {sector}-byte logical sectors"
        )));
    }
    let overlaps = inventory.entries.iter().any(|entry| {
        entry.device.parent.as_ref() == Some(&disk.device.path)
            && entry
                .device
                .start_bytes()
                .zip(entry.device.end_bytes())
                .is_some_and(|(existing_start, existing_end)| {
                    start < existing_end && end > existing_start
                })
    });
    if overlaps {
        return Err(MinfmError::Message(
            "the requested partition overlaps an existing partition".into(),
        ));
    }
    Ok(())
}

fn validate_label(label: Option<&str>) -> Result<()> {
    if label.is_some_and(|label| label.len() > 255 || label.chars().any(char::is_control)) {
        return Err(MinfmError::Message(
            "filesystem labels must be at most 255 bytes and contain no control characters".into(),
        ));
    }
    Ok(())
}

fn validate_flag(flag: &str) -> Result<()> {
    const FLAGS: &[&str] = &[
        "boot",
        "esp",
        "hidden",
        "legacy_boot",
        "lvm",
        "raid",
        "swap",
        "msftdata",
        "bios_grub",
    ];
    if FLAGS.contains(&flag) {
        Ok(())
    } else {
        Err(MinfmError::Message("unsupported partition flag".into()))
    }
}

fn validate_partition_name(name: &str) -> Result<()> {
    if name.len() > 72 || name.chars().any(char::is_control) {
        return Err(MinfmError::Message(
            "partition names must be at most 72 bytes and contain no control characters".into(),
        ));
    }
    Ok(())
}

fn validate_partition_type(type_id: &str) -> Result<()> {
    let valid_mbr = type_id.len() == 2
        && type_id
            .chars()
            .all(|character| character.is_ascii_hexdigit());
    let valid_gpt = type_id.len() == 36
        && type_id
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            });
    let valid = valid_mbr || valid_gpt;
    if valid {
        Ok(())
    } else {
        Err(MinfmError::Message(
            "partition type must be a two-digit hexadecimal MBR ID or an exact GPT GUID".into(),
        ))
    }
}

fn validate_backup_destination(destination: &Path) -> Result<()> {
    if destination.as_os_str().is_empty() || destination.exists() {
        return Err(MinfmError::Message(
            "choose a new backup file that does not already exist".into(),
        ));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(MinfmError::Message(
            "the backup destination directory does not exist".into(),
        ));
    }
    Ok(())
}

fn validate_new_image_destination(destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(MinfmError::Message(
            "the image destination already exists".into(),
        ));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let metadata = fs::metadata(parent)
        .map_err(|error| crate::error::io_error("could not inspect the image folder", error))?;
    if !metadata.is_dir() {
        return Err(MinfmError::Message(
            "the image folder is not a directory".into(),
        ));
    }
    Ok(())
}

fn command_plan(
    action: &PartitionAction,
    inventory: &PartitionInventory,
) -> Result<Vec<CommandSpec>> {
    let path = || action.target().path.as_os_str().to_owned();
    let bytes = |value: u64| OsString::from(format!("{value}B"));
    let commands = match action {
        PartitionAction::ChangeLuksPassphrase { .. }
        | PartitionAction::SetMountOptions { .. }
        | PartitionAction::SetEncryptionOptions { .. } => Vec::new(),
        PartitionAction::SmartReport { .. } => {
            let mut command = CommandSpec::elevated("smartctl", [OsString::from("--all"), path()]);
            // smartctl uses the upper five exit bits to report health findings;
            // those are valid report results, not command failures.
            command.accepted_codes = (0..=255).filter(|code| code & 0b111 == 0).collect();
            vec![command]
        }
        PartitionAction::SmartTest { extended, .. } => vec![CommandSpec::elevated(
            "smartctl",
            [
                OsString::from("--test"),
                OsString::from(if *extended { "long" } else { "short" }),
                path(),
            ],
        )],
        PartitionAction::DriveSetting { setting, .. } => {
            let argument = match setting {
                DriveSetting::Standby(value) => format!("-S{value}"),
                DriveSetting::PowerManagement(value) => format!("-B{value}"),
                DriveSetting::AcousticManagement(value) => format!("-M{value}"),
                DriveSetting::WriteCache(enabled) => {
                    format!("-W{}", if *enabled { 1 } else { 0 })
                }
            };
            vec![CommandSpec::elevated(
                "hdparm",
                [OsString::from(argument), path()],
            )]
        }
        PartitionAction::Mount { .. } => vec![CommandSpec::user(
            "udisksctl",
            [
                OsString::from("mount"),
                OsString::from("--block-device"),
                path(),
                OsString::from("--no-user-interaction"),
            ],
        )],
        PartitionAction::Unmount { .. } => vec![CommandSpec::user(
            "udisksctl",
            [
                OsString::from("unmount"),
                OsString::from("--block-device"),
                path(),
                OsString::from("--no-user-interaction"),
            ],
        )],
        PartitionAction::CreateTable {
            table,
            disk,
            overwrite,
        } => {
            let mut commands = wipe_disk_commands(disk, inventory);
            if *overwrite {
                commands.insert(0, full_overwrite_command(disk));
            }
            commands.push(CommandSpec::elevated(
                "parted",
                [
                    OsString::from("--script"),
                    path(),
                    OsString::from("mklabel"),
                    OsString::from(table.parted_name()),
                ],
            ));
            commands.push(reread_partition_table_command(disk));
            commands
        }
        PartitionAction::EraseDisk { disk, overwrite } => {
            let mut commands = wipe_disk_commands(disk, inventory);
            if *overwrite {
                commands.insert(0, full_overwrite_command(disk));
            }
            commands.push(reread_partition_table_command(disk));
            commands
        }
        PartitionAction::CreatePartition {
            start_bytes,
            end_bytes,
            disk,
        } => vec![
            CommandSpec::elevated(
                "parted",
                [
                    OsString::from("--script"),
                    OsString::from("--align"),
                    OsString::from("optimal"),
                    path(),
                    OsString::from("unit"),
                    OsString::from("B"),
                    OsString::from("mkpart"),
                    OsString::from("primary"),
                    bytes(*start_bytes),
                    bytes(*end_bytes),
                ],
            ),
            reread_partition_table_command(disk),
        ],
        PartitionAction::DeletePartition { number, disk, .. } => vec![CommandSpec::elevated(
            "parted",
            [
                OsString::from("--script"),
                disk.path.as_os_str().to_owned(),
                OsString::from("rm"),
                OsString::from(number.to_string()),
            ],
        )],
        PartitionAction::SetPartitionName {
            disk, number, name, ..
        } => vec![CommandSpec::elevated(
            "parted",
            [
                OsString::from("--script"),
                disk.path.as_os_str().to_owned(),
                OsString::from("name"),
                OsString::from(number.to_string()),
                OsString::from(name),
            ],
        )],
        PartitionAction::SetPartitionType {
            disk,
            number,
            type_id,
            ..
        } => vec![CommandSpec::elevated(
            "parted",
            [
                OsString::from("--script"),
                disk.path.as_os_str().to_owned(),
                OsString::from("type"),
                OsString::from(number.to_string()),
                OsString::from(type_id),
            ],
        )],
        PartitionAction::Format {
            filesystem, label, ..
        } => {
            let mut commands = vec![CommandSpec::elevated(
                "wipefs",
                [OsString::from("--all"), OsString::from("--force"), path()],
            )];
            if *filesystem != Filesystem::None {
                commands.push(format_command(path(), *filesystem, label.as_deref()));
            }
            commands
        }
        PartitionAction::EncryptFormat { .. } | PartitionAction::CreateEncryptedDisk { .. } => {
            Vec::new()
        }
        PartitionAction::Grow {
            disk,
            number,
            end_bytes,
            filesystem,
            ..
        } => {
            let mut commands = Vec::new();
            if *filesystem == Some(Filesystem::Ext4) {
                let mut check = CommandSpec::elevated("e2fsck", [OsString::from("-f"), path()]);
                check.accepted_codes = vec![0, 1];
                commands.push(check);
            }
            commands.push(CommandSpec::elevated(
                "parted",
                [
                    OsString::from("--script"),
                    disk.path.as_os_str().to_owned(),
                    OsString::from("unit"),
                    OsString::from("B"),
                    OsString::from("resizepart"),
                    OsString::from(number.to_string()),
                    bytes(*end_bytes),
                ],
            ));
            commands.push(reread_partition_table_command(disk));
            if *filesystem == Some(Filesystem::Ext4) {
                commands.push(CommandSpec::elevated("resize2fs", [path()]));
            }
            commands
        }
        PartitionAction::Shrink {
            disk,
            number,
            end_bytes,
            ..
        } => {
            let target_start = inventory
                .entries
                .iter()
                .find(|entry| entry.device.path == action.target().path)
                .and_then(|entry| entry.device.start_bytes())
                .ok_or_else(|| {
                    MinfmError::Message("the current partition boundary is unavailable".into())
                })?;
            let partition_size = end_bytes.saturating_sub(target_start);
            let filesystem_size_kib = partition_size
                .saturating_sub(1024 * 1024)
                .checked_div(1024)
                .ok_or_else(|| MinfmError::Message("the requested size is invalid".into()))?;
            let mut check = CommandSpec::elevated("e2fsck", [OsString::from("-f"), path()]);
            check.accepted_codes = vec![0, 1];
            vec![
                check,
                CommandSpec::elevated(
                    "resize2fs",
                    [path(), OsString::from(format!("{filesystem_size_kib}K"))],
                ),
                CommandSpec::elevated(
                    "parted",
                    [
                        OsString::from("--script"),
                        disk.path.as_os_str().to_owned(),
                        OsString::from("unit"),
                        OsString::from("B"),
                        OsString::from("resizepart"),
                        OsString::from(number.to_string()),
                        bytes(*end_bytes),
                    ],
                ),
                reread_partition_table_command(disk),
                CommandSpec::elevated("resize2fs", [path()]),
            ]
        }
        PartitionAction::SetLabel {
            filesystem, label, ..
        } => vec![label_command(path(), *filesystem, label)],
        PartitionAction::CheckFilesystem { filesystem, .. } => {
            vec![check_command(path(), *filesystem)]
        }
        PartitionAction::RepairFilesystem { filesystem, .. } => {
            vec![repair_command(path(), *filesystem)]
        }
        PartitionAction::SetFlag {
            disk,
            number,
            flag,
            enabled,
            ..
        } => vec![CommandSpec::elevated(
            "parted",
            [
                OsString::from("--script"),
                disk.path.as_os_str().to_owned(),
                OsString::from("set"),
                OsString::from(number.to_string()),
                OsString::from(flag),
                OsString::from(if *enabled { "on" } else { "off" }),
            ],
        )],
        PartitionAction::BackupTable { .. } => vec![CommandSpec::elevated(
            "sfdisk",
            [OsString::from("--dump"), path()],
        )],
        PartitionAction::CreateImage { destination, .. } => vec![
            CommandSpec::elevated(
                "dd",
                [
                    OsString::from(format!("if={}", action.target().path.display())),
                    OsString::from(format!("of={}", destination.display())),
                    OsString::from("bs=4M"),
                    OsString::from("iflag=fullblock"),
                    OsString::from("oflag=excl"),
                    OsString::from("status=progress"),
                    OsString::from("conv=fsync"),
                ],
            ),
            CommandSpec::elevated(
                "chown",
                [
                    OsString::from(format!("{}:{}", Uid::current(), Gid::current())),
                    destination.as_os_str().to_owned(),
                ],
            ),
        ],
        PartitionAction::RestoreImage { source, .. } => vec![
            CommandSpec::elevated(
                "dd",
                [
                    OsString::from(format!("if={}", source.display())),
                    OsString::from(format!("of={}", action.target().path.display())),
                    OsString::from("bs=4M"),
                    OsString::from("iflag=fullblock"),
                    OsString::from("oflag=direct"),
                    OsString::from("status=progress"),
                    OsString::from("conv=fsync"),
                ],
            ),
            CommandSpec::elevated("blockdev", [OsString::from("--rereadpt"), path()]),
        ],
    };
    Ok(commands)
}

fn full_overwrite_command(disk: &DeviceIdentity) -> CommandSpec {
    CommandSpec::elevated(
        "dd",
        [
            OsString::from("if=/dev/zero"),
            OsString::from(format!("of={}", disk.path.display())),
            OsString::from("bs=16M"),
            OsString::from("status=progress"),
            OsString::from("conv=fsync"),
        ],
    )
}

fn wipe_disk_commands(disk: &DeviceIdentity, inventory: &PartitionInventory) -> Vec<CommandSpec> {
    let mut partitions = inventory
        .entries
        .iter()
        .filter(|entry| {
            entry.device.kind == "part" && entry.device.parent.as_ref() == Some(&disk.path)
        })
        .map(|entry| entry.device.path.clone())
        .collect::<Vec<_>>();
    partitions.sort();
    let mut commands = partitions
        .into_iter()
        .map(|partition| {
            CommandSpec::elevated(
                "wipefs",
                [
                    OsString::from("--all"),
                    OsString::from("--force"),
                    partition.into_os_string(),
                ],
            )
        })
        .collect::<Vec<_>>();
    commands.push(CommandSpec::elevated(
        "wipefs",
        [
            OsString::from("--all"),
            OsString::from("--force"),
            disk.path.as_os_str().to_owned(),
        ],
    ));
    commands
}

fn reread_partition_table_command(disk: &DeviceIdentity) -> CommandSpec {
    CommandSpec::elevated(
        "blockdev",
        [
            OsString::from("--rereadpt"),
            disk.path.as_os_str().to_owned(),
        ],
    )
}

fn verify_final_state(action: &PartitionAction, inventory: &PartitionInventory) -> Result<()> {
    if let PartitionAction::DeletePartition { target, .. } = action {
        if inventory
            .entries
            .iter()
            .any(|entry| entry.device.path == target.path)
        {
            return Err(MinfmError::Message(format!(
                "{} still exists after deletion",
                target.path.display()
            )));
        }
        return Ok(());
    }
    let target = matching_entry(inventory, action.target())?;
    match action {
        PartitionAction::CreateTable { table, disk, .. } => {
            if !table.current_matches(target.device.table_type.as_deref()) {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested {} table",
                    disk.path.display(),
                    table.display_name()
                )));
            }
            require_no_partition_children(disk, inventory)?;
        }
        PartitionAction::EraseDisk { disk, .. } => {
            if target.device.table_type.is_some() {
                return Err(MinfmError::Message(format!(
                    "{} still reports a partition table",
                    disk.path.display()
                )));
            }
            require_no_partition_children(disk, inventory)?;
        }
        PartitionAction::Format { filesystem, .. }
            if !filesystem.current_matches(target.device.filesystem.as_deref()) =>
        {
            return Err(MinfmError::Message(format!(
                "{} does not report the requested {} filesystem",
                target.device.path.display(),
                filesystem.name()
            )));
        }
        PartitionAction::EncryptFormat { .. } => {
            if target.device.filesystem.as_deref() != Some("crypto_LUKS") {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested LUKS2 container",
                    target.device.path.display()
                )));
            }
        }
        PartitionAction::CreateEncryptedDisk { disk, .. } => {
            if target.device.table_type.as_deref() != Some("gpt") {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested GPT table",
                    disk.path.display()
                )));
            }
            let encrypted = inventory
                .entries
                .iter()
                .filter(|entry| {
                    entry.device.kind == "part"
                        && entry.device.parent.as_ref() == Some(&disk.path)
                        && entry.device.filesystem.as_deref() == Some("crypto_LUKS")
                })
                .count();
            if encrypted != 1 {
                return Err(MinfmError::Message(format!(
                    "{} does not report exactly one LUKS2 partition",
                    disk.path.display()
                )));
            }
        }
        PartitionAction::Grow { end_bytes, .. } | PartitionAction::Shrink { end_bytes, .. } => {
            let start = target.device.start_bytes().ok_or_else(|| {
                MinfmError::Message("the final partition boundary is unavailable".into())
            })?;
            let expected = end_bytes.saturating_sub(start);
            let tolerance = target.device.logical_sector_size.max(1);
            if target.device.size.abs_diff(expected) > tolerance {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested partition size",
                    target.device.path.display()
                )));
            }
        }
        PartitionAction::CreatePartition {
            disk,
            start_bytes,
            end_bytes,
        } => {
            let created = inventory.entries.iter().any(|entry| {
                entry.device.parent.as_ref() == Some(&disk.path)
                    && entry.device.start_bytes() == Some(*start_bytes)
                    && entry.device.end_bytes().is_some_and(|end| {
                        end.abs_diff(*end_bytes) <= entry.device.logical_sector_size.max(1)
                    })
            });
            if !created {
                return Err(MinfmError::Message(format!(
                    "{} does not report the newly created partition",
                    disk.path.display()
                )));
            }
        }
        PartitionAction::SetLabel { label, .. } => {
            let matches = if label.is_empty() {
                target
                    .device
                    .label
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
            } else {
                target.device.label.as_deref() == Some(label.as_str())
            };
            if !matches {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested filesystem label",
                    target.device.path.display()
                )));
            }
        }
        PartitionAction::SetPartitionName { name, .. } => {
            let matches = if name.is_empty() {
                target
                    .device
                    .partition_label
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
            } else {
                target.device.partition_label.as_deref() == Some(name.as_str())
            };
            if !matches {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested partition name",
                    target.device.path.display()
                )));
            }
        }
        PartitionAction::SetPartitionType { type_id, .. } => {
            if !target
                .device
                .partition_type
                .as_deref()
                .is_some_and(|current| {
                    current.eq_ignore_ascii_case(type_id)
                        || current
                            .strip_prefix("0x")
                            .is_some_and(|current| current.eq_ignore_ascii_case(type_id))
                })
            {
                return Err(MinfmError::Message(format!(
                    "{} does not report the requested partition type",
                    target.device.path.display()
                )));
            }
        }
        PartitionAction::SetFlag { .. } => {}
        _ => {}
    }
    Ok(())
}

fn verify_partition_flag(
    action: &PartitionAction,
    use_sudo: bool,
    administrator_password: Option<&[u8]>,
) -> Result<()> {
    let PartitionAction::SetFlag {
        disk,
        number,
        flag,
        enabled,
        ..
    } = action
    else {
        return Ok(());
    };
    let command = CommandSpec::elevated(
        "parted",
        [
            OsString::from("--machine"),
            OsString::from("--script"),
            disk.path.as_os_str().to_owned(),
            OsString::from("print"),
        ],
    );
    let output = run_command(&command, use_sudo, administrator_password)?;
    let text = std::str::from_utf8(&output)
        .map_err(|_| MinfmError::Message("parted returned invalid flag information".into()))?;
    let present = parted_flag_state(text, *number, flag)?;
    if present != *enabled {
        return Err(MinfmError::Message(format!(
            "{} does not report the requested {flag} flag state",
            action.target().path.display()
        )));
    }
    Ok(())
}

fn parted_flag_state(output: &str, number: u32, flag: &str) -> Result<bool> {
    let prefix = format!("{number}:");
    let line = output
        .lines()
        .find(|line| line.starts_with(&prefix))
        .ok_or_else(|| MinfmError::Message("parted no longer reports the partition".into()))?;
    let flags = line
        .trim_end_matches(';')
        .rsplit(':')
        .next()
        .unwrap_or_default();
    Ok(flags
        .split(',')
        .map(str::trim)
        .any(|current| current == flag))
}

fn require_no_partition_children(
    disk: &DeviceIdentity,
    inventory: &PartitionInventory,
) -> Result<()> {
    if let Some(child) = inventory.entries.iter().find(|entry| {
        entry.device.kind == "part" && entry.device.parent.as_ref() == Some(&disk.path)
    }) {
        return Err(MinfmError::Message(format!(
            "the kernel still reports old partition {}; close anything using it and retry",
            child.device.path.display()
        )));
    }
    Ok(())
}

fn format_command(path: OsString, filesystem: Filesystem, label: Option<&str>) -> CommandSpec {
    let (program, mut arguments) = match filesystem {
        Filesystem::Ext4 => ("mkfs.ext4", vec![OsString::from("-F")]),
        Filesystem::Ntfs => ("mkfs.ntfs", vec![OsString::from("-F")]),
        Filesystem::Xfs => ("mkfs.xfs", vec![OsString::from("-f")]),
        Filesystem::Btrfs => ("mkfs.btrfs", vec![OsString::from("-f")]),
        Filesystem::F2fs => ("mkfs.f2fs", vec![OsString::from("-f")]),
        Filesystem::Fat32 => ("mkfs.fat", vec![OsString::from("-F"), OsString::from("32")]),
        Filesystem::Exfat => ("mkfs.exfat", Vec::new()),
        Filesystem::Swap => ("mkswap", Vec::new()),
        Filesystem::Udf => ("mkudffs", Vec::new()),
        Filesystem::None => unreachable!("no-filesystem formatting only wipes signatures"),
    };
    if let Some(label) = label.filter(|label| !label.is_empty()) {
        arguments.push(OsString::from(match filesystem {
            Filesystem::Fat32 | Filesystem::Ntfs => "-n",
            Filesystem::Udf => "--lvid",
            _ => "-L",
        }));
        arguments.push(OsString::from(label));
    }
    arguments.push(path);
    CommandSpec::elevated(program, arguments)
}

fn label_command(path: OsString, filesystem: Filesystem, label: &str) -> CommandSpec {
    let (program, arguments) = match filesystem {
        Filesystem::Ext4 => ("e2label", vec![path, label.into()]),
        Filesystem::Ntfs => ("ntfslabel", vec![path, label.into()]),
        Filesystem::Xfs => ("xfs_admin", vec![OsString::from("-L"), label.into(), path]),
        Filesystem::Btrfs => (
            "btrfs",
            vec![
                OsString::from("filesystem"),
                OsString::from("label"),
                path,
                label.into(),
            ],
        ),
        Filesystem::F2fs => ("f2fslabel", vec![path, label.into()]),
        Filesystem::Fat32 => ("fatlabel", vec![path, label.into()]),
        Filesystem::Exfat => ("exfatlabel", vec![path, label.into()]),
        Filesystem::Swap => ("swaplabel", vec![OsString::from("-L"), label.into(), path]),
        Filesystem::Udf => ("udflabel", vec![path, label.into()]),
        Filesystem::None => unreachable!("an unformatted partition has no label"),
    };
    CommandSpec::elevated(program, arguments)
}

fn check_command(path: OsString, filesystem: Filesystem) -> CommandSpec {
    let (program, arguments) = match filesystem {
        Filesystem::Ext4 => ("e2fsck", vec![OsString::from("-fn"), path]),
        Filesystem::Ntfs => ("ntfsfix", vec![OsString::from("-n"), path]),
        Filesystem::Xfs => ("xfs_repair", vec![OsString::from("-n"), path]),
        Filesystem::Btrfs => (
            "btrfs",
            vec![OsString::from("check"), OsString::from("--readonly"), path],
        ),
        Filesystem::F2fs => ("fsck.f2fs", vec![OsString::from("-f"), path]),
        Filesystem::Fat32 => ("fsck.fat", vec![OsString::from("-n"), path]),
        Filesystem::Exfat => ("fsck.exfat", vec![OsString::from("-n"), path]),
        Filesystem::Swap => ("swaplabel", vec![path]),
        Filesystem::Udf => ("fsck.udf", vec![OsString::from("-n"), path]),
        Filesystem::None => unreachable!("an unformatted partition cannot be checked"),
    };
    CommandSpec::elevated(program, arguments)
}

fn repair_command(path: OsString, filesystem: Filesystem) -> CommandSpec {
    let (program, arguments) = match filesystem {
        Filesystem::Ext4 => (
            "e2fsck",
            vec![OsString::from("-f"), OsString::from("-p"), path],
        ),
        Filesystem::Ntfs => ("ntfsfix", vec![path]),
        Filesystem::Xfs => ("xfs_repair", vec![path]),
        Filesystem::Btrfs => (
            "btrfs",
            vec![OsString::from("check"), OsString::from("--repair"), path],
        ),
        Filesystem::F2fs => ("fsck.f2fs", vec![OsString::from("-f"), path]),
        Filesystem::Fat32 => ("fsck.fat", vec![OsString::from("-a"), path]),
        Filesystem::Exfat => ("fsck.exfat", vec![OsString::from("-p"), path]),
        Filesystem::Udf => ("fsck.udf", vec![OsString::from("-p"), path]),
        Filesystem::Swap | Filesystem::None => {
            unreachable!("unsupported repair filesystem was validated")
        }
    };
    let mut command = CommandSpec::elevated(program, arguments);
    if filesystem == Filesystem::Ext4 {
        command.accepted_codes = vec![0, 1, 2];
    }
    command
}

struct SudoSession {
    program: PathBuf,
}

impl Drop for SudoSession {
    fn drop(&mut self) {
        let _ = Command::new(&self.program)
            .arg("--reset-timestamp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn authenticate_sudo(password: &[u8]) -> Result<PathBuf> {
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

fn run_command(
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

fn run_command_with_secret<const N: usize>(
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

fn write_new_backup(destination: &Path, contents: &[u8]) -> Result<()> {
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

fn sudo_command_arguments(program: OsString, arguments: &[OsString]) -> Vec<OsString> {
    let mut sudo_arguments = Vec::with_capacity(arguments.len() + 5);
    sudo_arguments.push(OsString::from("--stdin"));
    sudo_arguments.push(OsString::from("--reset-timestamp"));
    sudo_arguments.push(OsString::from("--prompt="));
    sudo_arguments.push(OsString::from("--"));
    sudo_arguments.push(program);
    sudo_arguments.extend(arguments.iter().cloned());
    sudo_arguments
}

fn trusted_program(program: &OsString) -> Result<PathBuf> {
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

fn trusted_ownership_chain(path: &Path) -> bool {
    path.ancestors().skip(1).all(|ancestor| {
        fs::metadata(ancestor)
            .is_ok_and(|metadata| metadata.uid() == 0 && metadata.mode() & 0o022 == 0)
    })
}

fn settle_devices() {
    let _ = Command::new("udevadm")
        .args(["settle", "--timeout=10"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn parse_size(value: &str, disk_size: u64) -> Result<u64> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u64>()
            .map_err(|_| MinfmError::Message("invalid percentage".into()))?;
        if percent > 100 {
            return Err(MinfmError::Message(
                "percentage must be between 0 and 100".into(),
            ));
        }
        return disk_size
            .checked_mul(percent)
            .map(|value| value / 100)
            .ok_or_else(|| MinfmError::Message("size is too large".into()));
    }
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<u64>()
        .map_err(|_| MinfmError::Message("size must begin with a whole number".into()))?;
    let multiplier = match value[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        "tib" => 1024_u64.pow(4),
        _ => {
            return Err(MinfmError::Message(
                "use B, KiB, MiB, GiB, TiB, or %".into(),
            ))
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| MinfmError::Message("size is too large".into()))
}

pub fn size_input(value: u64) -> String {
    if value.is_multiple_of(1024 * 1024 * 1024) {
        format!("{}GiB", value / (1024 * 1024 * 1024))
    } else if value.is_multiple_of(1024 * 1024) {
        format!("{}MiB", value / (1024 * 1024))
    } else if value.is_multiple_of(1024) {
        format!("{}KiB", value / 1024)
    } else {
        format!("{value}B")
    }
}

pub fn free_regions(disk: &PartitionEntry, entries: &[PartitionEntry]) -> Vec<(u64, u64)> {
    if !disk.device.is_disk() || disk.device.size <= 2 * 1024 * 1024 {
        return Vec::new();
    }
    let margin = 1024 * 1024;
    let alignment = disk.device.logical_sector_size.max(1024 * 1024);
    let align_up = |value: u64| value.div_ceil(alignment) * alignment;
    let align_down = |value: u64| value - value % alignment;
    let mut extents = entries
        .iter()
        .filter(|entry| entry.device.parent.as_ref() == Some(&disk.device.path))
        .filter_map(|entry| entry.device.start_bytes().zip(entry.device.end_bytes()))
        .collect::<Vec<_>>();
    extents.sort_unstable();
    let mut regions = Vec::new();
    let mut cursor = align_up(margin);
    for (start, end) in extents {
        let region_end = align_down(start);
        if region_end.saturating_sub(cursor) >= margin {
            regions.push((cursor, region_end));
        }
        cursor = align_up(cursor.max(end));
    }
    let usable_end = align_down(disk.device.size.saturating_sub(margin));
    if usable_end.saturating_sub(cursor) >= margin {
        regions.push((cursor, usable_end));
    }
    regions
}

pub fn largest_free_region(
    disk: &PartitionEntry,
    entries: &[PartitionEntry],
) -> Option<(u64, u64)> {
    free_regions(disk, entries)
        .into_iter()
        .max_by_key(|(start, end)| end - start)
}

#[allow(dead_code)]
pub fn maximum_growth_end(partition: &PartitionEntry, entries: &[PartitionEntry]) -> Option<u64> {
    let parent = partition.device.parent.as_ref()?;
    let disk = entries
        .iter()
        .find(|entry| entry.device.path == *parent && entry.device.is_disk())?;
    let current_start = partition.device.start_bytes()?;
    let current_end = partition.device.end_bytes()?;
    let disk_end = disk.device.size.saturating_sub(1024 * 1024);
    let next_start = entries
        .iter()
        .filter(|entry| {
            entry.device.path != partition.device.path
                && entry.device.parent.as_ref() == Some(parent)
        })
        .filter_map(|entry| entry.device.start_bytes())
        .filter(|start| *start > current_start)
        .min();
    let maximum = next_start.unwrap_or(disk_end).min(disk_end);
    (maximum > current_end).then_some(maximum)
}

fn format_bytes(value: u64) -> String {
    if value.is_multiple_of(1024 * 1024 * 1024) {
        format!("{} GiB", value / (1024 * 1024 * 1024))
    } else if value.is_multiple_of(1024 * 1024) {
        format!("{} MiB", value / (1024 * 1024))
    } else {
        format!("{value} B")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn orders_disks_and_nested_partitions_as_a_tree() {
        let fixture = concat!(
            "PATH=\"/dev/sda2\" TYPE=\"part\" SIZE=\"90\" FSTYPE=\"ext4\" MOUNTPOINTS=\"/\" PKNAME=\"sda\" RO=\"0\" RM=\"0\"\n",
            "PATH=\"/dev/sda\" TYPE=\"disk\" SIZE=\"100\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" RO=\"0\" RM=\"0\"\n",
            "PATH=\"/dev/sda1\" TYPE=\"part\" SIZE=\"10\" FSTYPE=\"vfat\" MOUNTPOINTS=\"/boot\" PKNAME=\"sda\" RO=\"0\" RM=\"0\"\n",
        );
        let blocks = block::parse_lsblk(fixture, &[PathBuf::from("/dev/sda2")]).unwrap();
        let inventory = PartitionInventory::from_blocks(blocks);

        assert_eq!(inventory.entries[0].device.path, Path::new("/dev/sda"));
        assert_eq!(inventory.entries[1].device.path, Path::new("/dev/sda1"));
        assert_eq!(inventory.entries[2].device.path, Path::new("/dev/sda2"));
        assert_eq!(inventory.entries[0].depth, 0);
        assert_eq!(inventory.entries[1].depth, 1);
        assert!(inventory.entries.iter().all(|entry| entry.protected));
    }

    #[test]
    fn state_prioritizes_protection_and_read_only() {
        let fixture = "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"100\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" RO=\"1\" RM=\"1\"\n";
        let blocks = block::parse_lsblk(fixture, &[]).unwrap();
        let inventory = PartitionInventory::from_blocks(blocks);
        assert_eq!(inventory.entries[0].state_text(), "read only");
    }

    const OPERATION_FIXTURE: &str = concat!(
        "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:16\"\n",
        "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" LABEL=\"Data\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:17\"\n",
        "PATH=\"/dev/sdc\" TYPE=\"disk\" SIZE=\"536870912\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:32\"\n",
    );

    fn operation_inventory(protected: &[PathBuf]) -> PartitionInventory {
        PartitionInventory::from_blocks(block::parse_lsblk(OPERATION_FIXTURE, protected).unwrap())
    }

    fn identity(inventory: &PartitionInventory, path: &str) -> DeviceIdentity {
        let entry = inventory
            .entries
            .iter()
            .find(|entry| entry.device.path == Path::new(path))
            .unwrap();
        DeviceIdentity::from_entry(entry).unwrap()
    }

    fn arguments(spec: &CommandSpec) -> Vec<String> {
        spec.arguments
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn backup_creation_never_overwrites_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("table.sfdisk");
        write_new_backup(&destination, b"label: gpt\n").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"label: gpt\n");
        assert!(write_new_backup(&destination, b"replacement").is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"label: gpt\n");
    }

    #[test]
    fn partition_type_validation_accepts_only_exact_ids() {
        assert!(validate_partition_type("83").is_ok());
        assert!(validate_partition_type("0fc63daf-8483-4772-8e79-3d69d8477de4").is_ok());
        assert!(validate_partition_type("linux").is_err());
        assert!(validate_partition_type("83;reboot").is_err());
    }

    #[test]
    fn smart_and_drive_settings_use_explicit_safe_arguments() {
        let inventory = operation_inventory(&[]);
        let disk = identity(&inventory, "/dev/sdb");
        let report = command_plan(
            &PartitionAction::SmartReport { disk: disk.clone() },
            &inventory,
        )
        .unwrap();
        assert_eq!(report[0].program, "smartctl");
        assert_eq!(arguments(&report[0]), ["--all", "/dev/sdb"]);

        let test = command_plan(
            &PartitionAction::SmartTest {
                disk: disk.clone(),
                extended: true,
            },
            &inventory,
        )
        .unwrap();
        assert_eq!(arguments(&test[0]), ["--test", "long", "/dev/sdb"]);

        let cache = command_plan(
            &PartitionAction::DriveSetting {
                disk,
                setting: DriveSetting::WriteCache(false),
            },
            &inventory,
        )
        .unwrap();
        assert_eq!(cache[0].program, "hdparm");
        assert_eq!(arguments(&cache[0]), ["-W0", "/dev/sdb"]);
    }

    #[test]
    fn persistent_options_replace_only_the_selected_uuid() {
        let current = concat!(
            "# keep this comment\n",
            "UUID=old /srv/old ext4 defaults 0 2\n",
            "UUID=keep /srv/keep ext4 defaults 0 2\n",
        );
        let updated =
            replace_config_entry(current, "old", "UUID=old\t/srv/new\tauto\tnofail\t0\t0");
        assert!(updated.contains("# keep this comment"));
        assert!(updated.contains("UUID=old\t/srv/new"));
        assert!(updated.contains("UUID=keep /srv/keep"));
        assert!(!updated.contains("/srv/old"));
    }

    #[test]
    fn persistent_option_fields_reject_whitespace_and_shell_syntax() {
        assert!(validate_option_field("defaults,nofail").is_ok());
        assert!(validate_option_field("x-systemd.device-timeout=10s").is_ok());
        assert!(validate_option_field("defaults;reboot").is_err());
        assert!(validate_option_field("defaults nofail").is_err());
    }

    #[test]
    fn smart_report_is_reduced_to_useful_health_fields() {
        let report = concat!(
            "smartctl 7.4\n",
            "Device Model: Test Disk\n",
            "Serial Number: SECRET-SERIAL\n",
            "SMART overall-health self-assessment test result: PASSED\n",
            "190 Airflow_Temperature_Cel 0x0022 070 050 045 Old_age Always - 30\n",
            "A very long unrelated diagnostic line\n",
        );
        let summary = summarize_smart_report(report);
        assert!(summary.contains("Device Model: Test Disk"));
        assert!(summary.contains("PASSED"));
        assert!(summary.contains("Airflow_Temperature"));
        assert!(!summary.contains("unrelated diagnostic"));
    }

    #[test]
    fn luks_passphrase_action_validates_and_redacts_both_keys() {
        let fixture = "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"crypto_LUKS\" UUID=\"luks-id\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:17\"\n";
        let inventory = PartitionInventory::from_blocks(block::parse_lsblk(fixture, &[]).unwrap());
        let mut old = SecretInput::default();
        let mut new = SecretInput::default();
        for character in "old secret".chars() {
            old.push(character);
        }
        for character in "new secret".chars() {
            new.push(character);
        }
        let action = PartitionAction::ChangeLuksPassphrase {
            target: identity(&inventory, "/dev/sdb1"),
            old,
            new,
        };
        validate_action(&action, &inventory).unwrap();
        let rendered = format!("{action:?}");
        assert!(!rendered.contains("old secret"));
        assert!(!rendered.contains("new secret"));
        assert!(rendered.matches("[REDACTED]").count() >= 2);
    }

    #[test]
    fn parted_machine_output_verifies_partition_flags() {
        let output = concat!(
            "BYT;\n",
            "/dev/sdb:1073741824B:scsi:512:512:gpt:Disk:;\n",
            "1:1048576B:105906175B:104857600B:ext4:Data:boot, esp;\n",
        );
        assert!(parted_flag_state(output, 1, "boot").unwrap());
        assert!(parted_flag_state(output, 1, "esp").unwrap());
        assert!(!parted_flag_state(output, 1, "hidden").unwrap());
        assert!(parted_flag_state(output, 2, "boot").is_err());
    }

    #[test]
    fn parses_binary_sizes_and_percentages_strictly() {
        assert_eq!(parse_size("1MiB", 10_000_000).unwrap(), 1_048_576);
        assert_eq!(parse_size("2GiB", u64::MAX).unwrap(), 2_147_483_648);
        assert_eq!(parse_size("75%", 1_000).unwrap(), 750);
        assert!(parse_size("101%", 1_000).is_err());
        assert!(parse_size("1MB", 1_000).is_err());
        assert!(parse_size("1.5GiB", 1_000).is_err());
        assert_eq!(size_input(1024 * 1024), "1MiB");
        assert_eq!(size_input(1536), "1536B");
    }

    #[test]
    fn largest_region_excludes_existing_partitions_and_disk_margins() {
        let inventory = operation_inventory(&[]);
        let disk = inventory
            .entries
            .iter()
            .find(|entry| entry.device.path == Path::new("/dev/sdb"))
            .unwrap();
        assert_eq!(
            largest_free_region(disk, &inventory.entries),
            Some((105_906_176, 1_072_693_248))
        );
    }

    #[test]
    fn protected_identity_and_mounted_state_are_revalidated() {
        let protected = operation_inventory(&[PathBuf::from("/dev/sdb1")]);
        let action = PartitionAction::Format {
            target: identity(&protected, "/dev/sdb1"),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        assert!(validate_action(&action, &protected)
            .unwrap_err()
            .to_string()
            .contains("protected"));

        let mut changed = operation_inventory(&[]);
        let target = identity(&changed, "/dev/sdb1");
        changed
            .entries
            .iter_mut()
            .find(|entry| entry.device.path == Path::new("/dev/sdb1"))
            .unwrap()
            .device
            .major_minor = Some("8:99".into());
        let action = PartitionAction::Format {
            target,
            filesystem: Filesystem::Ext4,
            label: None,
        };
        assert!(validate_action(&action, &changed)
            .unwrap_err()
            .to_string()
            .contains("different kernel device"));
    }

    #[test]
    fn read_only_checks_and_backups_are_allowed_on_protected_storage() {
        let inventory = operation_inventory(&[PathBuf::from("/dev/sdb")]);
        let check = PartitionAction::CheckFilesystem {
            target: identity(&inventory, "/dev/sdb1"),
            filesystem: Filesystem::Ext4,
        };
        validate_action(&check, &inventory).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let backup = PartitionAction::BackupTable {
            disk: identity(&inventory, "/dev/sdb"),
            destination: temp.path().join("table.sfdisk"),
        };
        validate_action(&backup, &inventory).unwrap();
    }

    #[test]
    fn create_rejects_overlap_and_unaligned_boundaries() {
        let inventory = operation_inventory(&[]);
        let disk = identity(&inventory, "/dev/sdb");
        let overlapping = PartitionAction::CreatePartition {
            disk: disk.clone(),
            start_bytes: 2 * 1024 * 1024,
            end_bytes: 20 * 1024 * 1024,
        };
        assert!(validate_action(&overlapping, &inventory)
            .unwrap_err()
            .to_string()
            .contains("overlaps"));
        let unaligned = PartitionAction::CreatePartition {
            disk,
            start_bytes: 105_906_177,
            end_bytes: 200 * 1024 * 1024,
        };
        assert!(validate_action(&unaligned, &inventory)
            .unwrap_err()
            .to_string()
            .contains("align"));
    }

    #[test]
    fn table_actions_reject_a_mismatched_parent_disk() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::DeletePartition {
            target: identity(&inventory, "/dev/sdb1"),
            disk: identity(&inventory, "/dev/sdc"),
            number: 1,
        };
        assert!(validate_action(&action, &inventory)
            .unwrap_err()
            .to_string()
            .contains("expected disk"));
    }

    #[test]
    fn format_plan_uses_argument_vectors_without_a_shell() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::Format {
            target: identity(&inventory, "/dev/sdb1"),
            filesystem: Filesystem::Ext4,
            label: Some("Archive Disk".into()),
        };
        validate_action(&action, &inventory).unwrap();
        let plan = command_plan(&action, &inventory).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].program, OsString::from("wipefs"));
        assert_eq!(arguments(&plan[0]), ["--all", "--force", "/dev/sdb1"]);
        assert_eq!(plan[1].program, OsString::from("mkfs.ext4"));
        assert_eq!(
            arguments(&plan[1]),
            ["-F", "-L", "Archive Disk", "/dev/sdb1"]
        );
        assert!(plan.iter().all(|command| command.elevated));
    }

    #[test]
    fn replacing_a_table_wipes_partition_and_disk_signatures_first() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::CreateTable {
            disk: identity(&inventory, "/dev/sdb"),
            table: PartitionTable::Gpt,
            overwrite: false,
        };
        validate_action(&action, &inventory).unwrap();
        let plan = command_plan(&action, &inventory).unwrap();
        assert_eq!(
            plan.iter()
                .map(|command| command.program.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["wipefs", "wipefs", "parted", "blockdev"]
        );
        assert_eq!(arguments(&plan[0]).last().unwrap(), "/dev/sdb1");
        assert_eq!(arguments(&plan[1]).last().unwrap(), "/dev/sdb");
        assert!(arguments(&plan[0]).contains(&"--force".to_string()));
        assert_eq!(arguments(&plan[3]), ["--rereadpt", "/dev/sdb"]);
    }

    #[test]
    fn leaving_a_disk_empty_wipes_children_and_disk_without_creating_a_table() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::EraseDisk {
            disk: identity(&inventory, "/dev/sdb"),
            overwrite: false,
        };
        validate_action(&action, &inventory).unwrap();
        let plan = command_plan(&action, &inventory).unwrap();
        assert_eq!(
            plan.iter()
                .map(|command| command.program.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["wipefs", "wipefs", "blockdev"]
        );
        assert_eq!(arguments(&plan[0]).last().unwrap(), "/dev/sdb1");
        assert_eq!(arguments(&plan[1]).last().unwrap(), "/dev/sdb");
        assert_eq!(arguments(&plan[2]), ["--rereadpt", "/dev/sdb"]);
    }

    #[test]
    fn final_state_rejects_a_stale_partition_after_disk_reset() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::CreateTable {
            disk: identity(&inventory, "/dev/sdb"),
            table: PartitionTable::Gpt,
            overwrite: false,
        };
        let error = verify_final_state(&action, &inventory).unwrap_err();
        assert!(error.to_string().contains("still reports old partition"));
        assert!(error.to_string().contains("/dev/sdb1"));
    }

    #[test]
    fn final_state_confirms_an_empty_disk_and_the_requested_filesystem() {
        let empty_fixture = "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" RM=\"1\" MAJ:MIN=\"8:16\"\n";
        let empty =
            PartitionInventory::from_blocks(block::parse_lsblk(empty_fixture, &[]).unwrap());
        let erase = PartitionAction::EraseDisk {
            disk: identity(&empty, "/dev/sdb"),
            overwrite: false,
        };
        verify_final_state(&erase, &empty).unwrap();

        let inventory = operation_inventory(&[]);
        let format = PartitionAction::Format {
            target: identity(&inventory, "/dev/sdb1"),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        verify_final_state(&format, &inventory).unwrap();
    }

    #[test]
    fn confirmation_warnings_distinguish_erasing_from_layout_changes() {
        let inventory = operation_inventory(&[]);
        let format = PartitionAction::Format {
            target: identity(&inventory, "/dev/sdb1"),
            filesystem: Filesystem::Ext4,
            label: None,
        };
        assert!(format.erases_data());
        assert_eq!(
            format.warning_text(),
            "This permanently erases data on the selected device."
        );

        let create = PartitionAction::CreatePartition {
            disk: identity(&inventory, "/dev/sdb"),
            start_bytes: 200 * 1024 * 1024,
            end_bytes: 300 * 1024 * 1024,
        };
        assert!(!create.erases_data());
        assert!(create.warning_text().contains("changes the disk layout"));
    }

    #[test]
    fn every_partition_action_builds_an_explicit_command_plan() {
        let inventory = operation_inventory(&[]);
        let disk = identity(&inventory, "/dev/sdb");
        let target = identity(&inventory, "/dev/sdb1");
        let cases = vec![
            (
                PartitionAction::Mount {
                    target: target.clone(),
                },
                "udisksctl",
            ),
            (
                PartitionAction::Unmount {
                    target: target.clone(),
                },
                "udisksctl",
            ),
            (
                PartitionAction::CreateTable {
                    disk: disk.clone(),
                    table: PartitionTable::Gpt,
                    overwrite: false,
                },
                "parted",
            ),
            (
                PartitionAction::EraseDisk {
                    disk: disk.clone(),
                    overwrite: false,
                },
                "wipefs",
            ),
            (
                PartitionAction::CreatePartition {
                    disk: disk.clone(),
                    start_bytes: 200 * 1024 * 1024,
                    end_bytes: 300 * 1024 * 1024,
                },
                "parted",
            ),
            (
                PartitionAction::DeletePartition {
                    target: target.clone(),
                    disk: disk.clone(),
                    number: 1,
                },
                "parted",
            ),
            (
                PartitionAction::SetPartitionName {
                    target: target.clone(),
                    disk: disk.clone(),
                    number: 1,
                    name: "Data".into(),
                },
                "parted",
            ),
            (
                PartitionAction::SetPartitionType {
                    target: target.clone(),
                    disk: disk.clone(),
                    number: 1,
                    type_id: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                },
                "parted",
            ),
            (
                PartitionAction::SetLabel {
                    target: target.clone(),
                    filesystem: Filesystem::Ext4,
                    label: "Data".into(),
                },
                "e2label",
            ),
            (
                PartitionAction::CheckFilesystem {
                    target: target.clone(),
                    filesystem: Filesystem::Ext4,
                },
                "e2fsck",
            ),
            (
                PartitionAction::SetFlag {
                    target: target.clone(),
                    disk: disk.clone(),
                    number: 1,
                    flag: "boot".into(),
                    enabled: true,
                },
                "parted",
            ),
            (
                PartitionAction::BackupTable {
                    disk: disk.clone(),
                    destination: PathBuf::from("table.sfdisk"),
                },
                "sfdisk",
            ),
        ];
        for (action, expected_program) in cases {
            let plan = command_plan(&action, &inventory).unwrap();
            assert!(!plan.is_empty(), "missing plan for {action:?}");
            assert!(
                plan.iter()
                    .any(|command| command.program == expected_program),
                "missing {expected_program} in plan for {action:?}"
            );
            assert!(plan
                .iter()
                .flat_map(|command| &command.arguments)
                .any(|argument| argument == "/dev/sdb" || argument == "/dev/sdb1"));
        }
    }

    #[test]
    fn filesystem_formatters_use_expected_helpers_and_label_switches() {
        let inventory = PartitionInventory {
            entries: Vec::new(),
        };
        let target = DeviceIdentity {
            path: PathBuf::from("/dev/test1"),
            major_minor: "1:2".into(),
        };
        let cases = [
            (Filesystem::Ext4, "mkfs.ext4", "-L"),
            (Filesystem::Xfs, "mkfs.xfs", "-L"),
            (Filesystem::Btrfs, "mkfs.btrfs", "-L"),
            (Filesystem::Fat32, "mkfs.fat", "-n"),
            (Filesystem::Exfat, "mkfs.exfat", "-L"),
            (Filesystem::Swap, "mkswap", "-L"),
        ];
        for (filesystem, expected_program, expected_label_switch) in cases {
            let action = PartitionAction::Format {
                target: target.clone(),
                filesystem,
                label: Some("Label".into()),
            };
            let plan = command_plan(&action, &inventory).unwrap();
            assert_eq!(plan[0].program, OsString::from("wipefs"));
            assert_eq!(plan[1].program, OsString::from(expected_program));
            assert!(arguments(&plan[1]).contains(&expected_label_switch.to_string()));
            assert_eq!(arguments(&plan[1]).last().unwrap(), "/dev/test1");
        }
    }

    #[test]
    fn ext4_growth_checks_then_grows_partition_and_filesystem() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::Grow {
            target: identity(&inventory, "/dev/sdb1"),
            disk: identity(&inventory, "/dev/sdb"),
            number: 1,
            end_bytes: 512 * 1024 * 1024,
            filesystem: Some(Filesystem::Ext4),
        };
        validate_action(&action, &inventory).unwrap();
        let plan = command_plan(&action, &inventory).unwrap();
        assert_eq!(
            plan.iter()
                .map(|command| command.program.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["e2fsck", "parted", "blockdev", "resize2fs"]
        );
        assert_eq!(plan[0].accepted_codes, [0, 1]);
    }

    #[test]
    fn growth_stops_before_the_next_partition() {
        let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:16\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:17\"\n",
            "PATH=\"/dev/sdb2\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"2\" START=\"409600\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:18\"\n",
        );
        let inventory = from_lsblk_fixture(fixture, &[]).unwrap();
        let first = inventory
            .entries
            .iter()
            .find(|entry| entry.device.path == Path::new("/dev/sdb1"))
            .unwrap();
        assert_eq!(
            maximum_growth_end(first, &inventory.entries),
            Some(209_715_200)
        );
        let action = PartitionAction::Grow {
            target: identity(&inventory, "/dev/sdb1"),
            disk: identity(&inventory, "/dev/sdb"),
            number: 1,
            end_bytes: 300 * 1024 * 1024,
            filesystem: Some(Filesystem::Ext4),
        };
        assert!(validate_action(&action, &inventory)
            .unwrap_err()
            .to_string()
            .contains("overlap"));
    }

    #[test]
    fn ext4_shrink_reduces_the_filesystem_before_the_partition() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::Shrink {
            target: identity(&inventory, "/dev/sdb1"),
            disk: identity(&inventory, "/dev/sdb"),
            number: 1,
            end_bytes: 65 * 1024 * 1024,
            filesystem: Filesystem::Ext4,
        };
        validate_action(&action, &inventory).unwrap();
        let plan = command_plan(&action, &inventory).unwrap();
        assert_eq!(
            plan.iter()
                .map(|command| command.program.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["e2fsck", "resize2fs", "parted", "blockdev", "resize2fs"]
        );
        assert_eq!(arguments(&plan[1]), ["/dev/sdb1", "64512K"]);
        assert_eq!(arguments(&plan[3]), ["--rereadpt", "/dev/sdb"]);
        assert_eq!(arguments(&plan[4]), ["/dev/sdb1"]);
    }

    #[test]
    fn free_regions_returns_each_gap_in_disk_order() {
        let fixture = concat!(
            "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"1073741824\" FSTYPE=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTN=\"\" START=\"0\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:16\"\n",
            "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"1\" START=\"2048\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:17\"\n",
            "PATH=\"/dev/sdb2\" TYPE=\"part\" SIZE=\"104857600\" FSTYPE=\"ext4\" MOUNTPOINTS=\"\" PKNAME=\"sdb\" PARTN=\"2\" START=\"409600\" LOG-SEC=\"512\" RO=\"0\" MAJ:MIN=\"8:18\"\n",
        );
        let inventory = from_lsblk_fixture(fixture, &[]).unwrap();
        let disk = &inventory.entries[0];
        assert_eq!(
            free_regions(disk, &inventory.entries),
            vec![
                (101 * 1024 * 1024, 200 * 1024 * 1024),
                (300 * 1024 * 1024, 1023 * 1024 * 1024),
            ]
        );
    }

    #[test]
    fn unsupported_partition_flags_are_rejected_by_the_safety_core() {
        let inventory = operation_inventory(&[]);
        let action = PartitionAction::SetFlag {
            target: identity(&inventory, "/dev/sdb1"),
            disk: identity(&inventory, "/dev/sdb"),
            number: 1,
            flag: "--script".into(),
            enabled: true,
        };
        assert!(validate_action(&action, &inventory)
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }

    #[test]
    fn privileged_helper_resolution_rejects_supplied_paths() {
        assert!(trusted_program(&OsString::from("./parted")).is_err());
        assert!(trusted_program(&OsString::from("/tmp/parted")).is_err());
    }

    #[test]
    fn privileged_commands_receive_password_input_instead_of_relying_on_a_timestamp() {
        let arguments = sudo_command_arguments(
            OsString::from("/usr/bin/wipefs"),
            &[
                OsString::from("--all"),
                OsString::from("--force"),
                OsString::from("/dev/sdb1"),
            ],
        );
        let rendered = arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            [
                "--stdin",
                "--reset-timestamp",
                "--prompt=",
                "--",
                "/usr/bin/wipefs",
                "--all",
                "--force",
                "/dev/sdb1",
            ]
        );
        assert!(!rendered.contains(&"--non-interactive".to_string()));
    }
}
