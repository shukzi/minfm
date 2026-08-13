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

mod geometry;
mod plan;
mod process;
mod validate;

use geometry::format_bytes;
pub use geometry::{free_regions, largest_free_region, maximum_growth_end, parse_size, size_input};
use plan::*;
use process::*;
pub use validate::validate_snapshot;
use validate::*;

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
    if let Some(filesystem) = newly_created_filesystem(action) {
        ensure_ownership_helpers(filesystem)?;
    }
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
        let report = complete_smart_report(&String::from_utf8_lossy(&output));
        return Ok(report);
    } else if matches!(action, PartitionAction::SmartTest { .. }) {
        let report_command = smart_report_command(action.target());
        report_phase("Checking current SMART test status");
        let current_output = run_command(&report_command, use_sudo, administrator_password)?;
        let current_report = String::from_utf8_lossy(&current_output);
        if smart_self_test_running(&current_report) {
            return Ok(format!(
                "A SMART self-test is already running.\n\n{}",
                complete_smart_report(&current_report)
            ));
        }
        report_phase("Starting SMART self-test");
        if let Err(error) = run_command(&commands[0], use_sudo, administrator_password) {
            if !smart_test_already_running_error(&error.to_string()) {
                return Err(error);
            }
            report_phase("Reading active SMART test status");
            let output = run_command(&report_command, use_sudo, administrator_password)?;
            return Ok(format!(
                "A SMART self-test is already running.\n\n{}",
                complete_smart_report(&String::from_utf8_lossy(&output))
            ));
        }
        report_phase("Reading SMART test status");
        let output = run_command(&report_command, use_sudo, administrator_password)?;
        let report = complete_smart_report(&String::from_utf8_lossy(&output));
        return Ok(format!("{} started.\n\n{}", action.title(), report));
    } else {
        for command in commands {
            report_phase(command_phase(&command));
            let output = run_command(&command, use_sudo, administrator_password)?;
            if let PartitionAction::BackupTable { destination, .. } = action {
                write_new_backup(destination, &output)?;
            }
        }
        if let PartitionAction::Format {
            target, filesystem, ..
        } = action
        {
            if take_ownership_supported(*filesystem) {
                report_phase("Assigning filesystem ownership");
                take_filesystem_ownership(
                    &target.path,
                    *filesystem,
                    use_sudo,
                    administrator_password,
                )?;
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
    const FIELDS: [&str; 14] = [
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
        "Self-test status",
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

fn smart_report_command(target: &DeviceIdentity) -> CommandSpec {
    CommandSpec {
        program: "smartctl".into(),
        arguments: vec!["--all".into(), target.path.as_os_str().to_os_string()],
        elevated: true,
        accepted_codes: (0..=255).filter(|code| code & 0b111 == 0).collect(),
    }
}

fn smart_self_test_running(report: &str) -> bool {
    report.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("self-test")
            && line.contains("in progress")
            && !line.contains("no self-test in progress")
            && !line.contains("not in progress")
    })
}

fn smart_test_already_running_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("can't start self-test without aborting current test")
        || error.contains("cannot start self-test without aborting current test")
}

fn complete_smart_report(report: &str) -> String {
    let report = report.trim();
    if report.is_empty() {
        return "smartctl returned an empty report.".into();
    }
    let summary = summarize_smart_report(report);
    format!("Summary\n{summary}\n\nFull report\n{report}")
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
        &format_command(mapping.as_os_str().to_owned(), filesystem, label),
        use_sudo,
        administrator_password,
    );
    let access_result = if format_result.is_ok() {
        report_phase("Assigning filesystem ownership");
        take_filesystem_ownership(&mapping, filesystem, use_sudo, administrator_password)
    } else {
        Ok(())
    };
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
    access_result?;
    close_result?;
    Ok(())
}

fn take_filesystem_ownership(
    device: &Path,
    filesystem: Filesystem,
    use_sudo: bool,
    administrator_password: Option<&[u8]>,
) -> Result<()> {
    if !take_ownership_supported(filesystem) {
        return Ok(());
    }

    let mountpoint = tempfile::Builder::new()
        .prefix("minfm-filesystem-access-")
        .tempdir()
        .map_err(|error| crate::error::io_error("could not prepare filesystem access", error))?;
    let commands = ownership_commands(device, mountpoint.path(), Uid::current(), Gid::current());
    run_command(&commands[0], use_sudo, administrator_password)?;
    let ownership_result = run_command(&commands[1], use_sudo, administrator_password);
    let unmount_result = run_command(&commands[2], use_sudo, administrator_password);
    match (ownership_result, unmount_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
        (Err(ownership_error), Err(unmount_error)) => Err(MinfmError::Message(format!(
            "{ownership_error}; additionally failed to unmount the temporary filesystem: {unmount_error}"
        ))),
    }
}

fn take_ownership_supported(filesystem: Filesystem) -> bool {
    matches!(
        filesystem,
        Filesystem::Ext4 | Filesystem::Xfs | Filesystem::Btrfs | Filesystem::F2fs | Filesystem::Udf
    )
}

fn newly_created_filesystem(action: &PartitionAction) -> Option<Filesystem> {
    match action {
        PartitionAction::Format { filesystem, .. }
        | PartitionAction::EncryptFormat { filesystem, .. }
        | PartitionAction::CreateEncryptedDisk { filesystem, .. } => Some(*filesystem),
        _ => None,
    }
}

fn ensure_ownership_helpers(filesystem: Filesystem) -> Result<()> {
    if take_ownership_supported(filesystem) {
        for helper in ["mount", "chown", "umount"] {
            let _ = trusted_program(&OsString::from(helper))?;
        }
    }
    Ok(())
}

fn ownership_commands(
    device: &Path,
    mountpoint: &Path,
    owner: Uid,
    group: Gid,
) -> [CommandSpec; 3] {
    [
        CommandSpec::elevated(
            "mount",
            [
                OsString::from("--options"),
                OsString::from("nosuid,nodev,noexec"),
                OsString::from("--"),
                device.as_os_str().to_owned(),
                mountpoint.as_os_str().to_owned(),
            ],
        ),
        CommandSpec::elevated(
            "chown",
            [
                OsString::from(format!("{owner}:{group}")),
                OsString::from("--"),
                mountpoint.as_os_str().to_owned(),
            ],
        ),
        CommandSpec::elevated(
            "umount",
            [OsString::from("--"), mountpoint.as_os_str().to_owned()],
        ),
    ]
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

#[allow(dead_code)]
#[cfg(test)]
mod tests;
