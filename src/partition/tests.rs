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
    assert!(test[0].accepted_codes.contains(&8));
    assert!(test[0].accepted_codes.contains(&128));
    assert!(!test[0].accepted_codes.contains(&1));

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
    let updated = replace_config_entry(current, "old", "UUID=old\t/srv/new\tauto\tnofail\t0\t0");
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

    let complete = complete_smart_report(report);
    assert!(complete.contains("Summary"));
    assert!(complete.contains("Full report"));
    assert!(complete.contains("unrelated diagnostic"));
}

#[test]
fn active_smart_tests_are_reported_instead_of_treated_as_failures() {
    let nvme_report = concat!(
        "SMART overall-health self-assessment test result: PASSED\n",
        "Self-test status: Extended self-test in progress (68% completed)\n",
    );
    assert!(smart_self_test_running(nvme_report));
    assert!(!smart_self_test_running(
        "Self-test status: No self-test in progress"
    ));
    assert!(smart_test_already_running_error(
        "smartctl failed: Can't start self-test without aborting current test (68% completed)"
    ));
    let summary = summarize_smart_report(nvme_report);
    assert!(summary.contains("68% completed"));
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
        permissions: FilesystemPermissions::SystemDefault,
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
        permissions: FilesystemPermissions::SystemDefault,
    };
    assert!(validate_action(&action, &changed)
        .unwrap_err()
        .to_string()
        .contains("different kernel device"));
}

#[test]
fn filesystem_permission_policy_only_changes_supported_filesystems_on_request() {
    for filesystem in [
        Filesystem::Ext4,
        Filesystem::Xfs,
        Filesystem::Btrfs,
        Filesystem::F2fs,
        Filesystem::Udf,
    ] {
        assert!(permission_change_supported(filesystem));
    }
    for filesystem in [
        Filesystem::Fat32,
        Filesystem::Exfat,
        Filesystem::Ntfs,
        Filesystem::Swap,
        Filesystem::None,
    ] {
        assert!(!permission_change_supported(filesystem));
        assert_eq!(
            FilesystemPermissions::Everyone.effective_for(filesystem),
            FilesystemPermissions::SystemDefault
        );
    }

    assert_eq!(
        FilesystemPermissions::Everyone.effective_for(Filesystem::Ext4),
        FilesystemPermissions::Everyone
    );

    let inventory = operation_inventory(&[]);
    let conventional = PartitionAction::Format {
        target: identity(&inventory, "/dev/sdb1"),
        filesystem: Filesystem::Ext4,
        label: None,
        permissions: FilesystemPermissions::SystemDefault,
    };
    assert_eq!(filesystem_needing_permission_change(&conventional), None);
    let everyone = PartitionAction::Format {
        target: identity(&inventory, "/dev/sdb1"),
        filesystem: Filesystem::Ext4,
        label: None,
        permissions: FilesystemPermissions::Everyone,
    };
    assert_eq!(
        filesystem_needing_permission_change(&everyone),
        Some(Filesystem::Ext4)
    );

    let commands = permission_commands(
        Path::new("/dev/mapper/test-volume"),
        Path::new("/tmp/test-mountpoint"),
    );
    assert_eq!(commands[0].program, OsString::from("mount"));
    assert_eq!(commands[1].program, OsString::from("chmod"));
    assert_eq!(commands[1].arguments[0], OsString::from("0777"));
    assert_eq!(commands[2].program, OsString::from("umount"));
    assert!(commands.iter().all(|command| command.elevated));
    assert!(commands.iter().all(|command| {
        command
            .arguments
            .iter()
            .any(|argument| argument == "/tmp/test-mountpoint")
    }));
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
        permissions: FilesystemPermissions::SystemDefault,
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
    let empty = PartitionInventory::from_blocks(block::parse_lsblk(empty_fixture, &[]).unwrap());
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
        permissions: FilesystemPermissions::SystemDefault,
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
        permissions: FilesystemPermissions::SystemDefault,
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
            permissions: FilesystemPermissions::SystemDefault,
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
