use super::*;

pub(super) fn command_plan(
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
        PartitionAction::SmartTest { extended, .. } => {
            let mut command = CommandSpec::elevated(
                "smartctl",
                [
                    OsString::from("--test"),
                    OsString::from(if *extended { "long" } else { "short" }),
                    path(),
                ],
            );
            // Health findings use the upper exit bits and do not mean that
            // starting the self-test failed.
            command.accepted_codes = (0..=255).filter(|code| code & 0b111 == 0).collect();
            vec![command]
        }
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

pub(super) fn full_overwrite_command(disk: &DeviceIdentity) -> CommandSpec {
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

pub(super) fn wipe_disk_commands(
    disk: &DeviceIdentity,
    inventory: &PartitionInventory,
) -> Vec<CommandSpec> {
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

pub(super) fn reread_partition_table_command(disk: &DeviceIdentity) -> CommandSpec {
    CommandSpec::elevated(
        "blockdev",
        [
            OsString::from("--rereadpt"),
            disk.path.as_os_str().to_owned(),
        ],
    )
}

pub(super) fn verify_final_state(
    action: &PartitionAction,
    inventory: &PartitionInventory,
) -> Result<()> {
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

pub(super) fn verify_partition_flag(
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

pub(super) fn parted_flag_state(output: &str, number: u32, flag: &str) -> Result<bool> {
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

pub(super) fn require_no_partition_children(
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

pub(super) fn format_command(
    path: OsString,
    filesystem: Filesystem,
    label: Option<&str>,
) -> CommandSpec {
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

pub(super) fn label_command(path: OsString, filesystem: Filesystem, label: &str) -> CommandSpec {
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

pub(super) fn check_command(path: OsString, filesystem: Filesystem) -> CommandSpec {
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

pub(super) fn repair_command(path: OsString, filesystem: Filesystem) -> CommandSpec {
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
