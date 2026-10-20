use super::*;

pub fn validate_snapshot(action: &PartitionAction, entries: &[PartitionEntry]) -> Result<()> {
    validate_action(
        action,
        &PartitionInventory {
            entries: entries.to_vec(),
        },
    )
}

pub(super) fn validate_action(
    action: &PartitionAction,
    inventory: &PartitionInventory,
) -> Result<()> {
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

pub(super) fn matching_entry<'a>(
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

pub(super) fn require_disk(entry: &PartitionEntry) -> Result<()> {
    if !entry.device.is_disk() {
        return Err(MinfmError::Message(
            "the selected target is not a disk".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_partition(entry: &PartitionEntry, number: u32) -> Result<()> {
    if entry.device.kind != "part" || entry.device.partition_number != Some(number) {
        return Err(MinfmError::Message(
            "the selected partition number changed".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_parent_disk(partition: &PartitionEntry, disk: &PartitionEntry) -> Result<()> {
    require_disk(disk)?;
    if partition.device.parent.as_ref() != Some(&disk.device.path) {
        return Err(MinfmError::Message(
            "the partition no longer belongs to the expected disk".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_safe_disk_for_table_change(disk: &PartitionEntry) -> Result<()> {
    if disk.protected || disk.device.read_only || disk.mounted_descendants {
        return Err(MinfmError::Message(
            "the parent disk is protected, read only, or contains mounted storage".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_inactive(entry: &PartitionEntry) -> Result<()> {
    if entry.device.is_mounted() || entry.mounted_descendants {
        return Err(MinfmError::Message(format!(
            "{} or one of its children is mounted",
            entry.device.path.display()
        )));
    }
    Ok(())
}

pub(super) fn require_no_mapped_descendants(
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

pub(super) fn is_descendant_of(
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

pub(super) fn require_filesystem(entry: &PartitionEntry, filesystem: Filesystem) -> Result<()> {
    if !filesystem.current_matches(entry.device.filesystem.as_deref()) {
        return Err(MinfmError::Message(
            "the filesystem type changed before the operation started".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_new_extent(
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

pub(super) fn validate_label(label: Option<&str>) -> Result<()> {
    if label.is_some_and(|label| label.len() > 255 || label.chars().any(char::is_control)) {
        return Err(MinfmError::Message(
            "filesystem labels must be at most 255 bytes and contain no control characters".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_flag(flag: &str) -> Result<()> {
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

pub(super) fn validate_partition_name(name: &str) -> Result<()> {
    if name.len() > 72 || name.chars().any(char::is_control) {
        return Err(MinfmError::Message(
            "partition names must be at most 72 bytes and contain no control characters".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_partition_type(type_id: &str) -> Result<()> {
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

pub(super) fn validate_backup_destination(destination: &Path) -> Result<()> {
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

pub(super) fn validate_new_image_destination(destination: &Path) -> Result<()> {
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
