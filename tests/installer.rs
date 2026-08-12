use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

struct InstallerFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    fake_bin: PathBuf,
    package_log: PathBuf,
}

impl InstallerFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let fake_bin = temp.path().join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir(&fake_bin).unwrap();

        for command in [
            "awk",
            "blockdev",
            "btrfs",
            "chown",
            "chmod",
            "cp",
            "cryptsetup",
            "dd",
            "e2fsck",
            "e2label",
            "exfatlabel",
            "f2fslabel",
            "fatlabel",
            "findmnt",
            "fsck.exfat",
            "fsck.f2fs",
            "fsck.fat",
            "gio",
            "grep",
            "hdparm",
            "install",
            "lsblk",
            "mkdir",
            "mkfs.btrfs",
            "mkfs.exfat",
            "mkfs.ext4",
            "mkfs.f2fs",
            "mkfs.fat",
            "mkfs.ntfs",
            "mkfs.xfs",
            "mkswap",
            "mktemp",
            "mkudffs",
            "mount",
            "mv",
            "ntfsfix",
            "ntfslabel",
            "parted",
            "resize2fs",
            "rm",
            "secret-tool",
            "sfdisk",
            "sha256sum",
            "smartctl",
            "swaplabel",
            "udisksctl",
            "umount",
            "uname",
            "wipefs",
            "xdg-open",
            "xfs_admin",
            "xfs_repair",
        ] {
            let source = Path::new("/usr/bin").join(command);
            if source.exists() {
                std::os::unix::fs::symlink(source, fake_bin.join(command)).unwrap();
            } else {
                write_executable(&fake_bin.join(command), "#!/bin/sh\nexit 0\n");
            }
        }
        write_executable(&fake_bin.join("fc-cache"), "#!/bin/sh\nexit 0\n");
        write_executable(
            &fake_bin.join("curl"),
            r#"#!/bin/sh
destination=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        destination=$1
    fi
    shift
done
case "$destination" in
    *.sha256)
        hash=$(printf 'fake-minfm\n' | sha256sum | awk '{print $1}')
        printf '%s  minfm-linux-x86_64\n' "$hash" > "$destination"
        ;;
    *) printf 'fake-minfm\n' > "$destination" ;;
esac
"#,
        );
        let package_log = temp.path().join("packages.log");
        write_executable(
            &fake_bin.join("sudo"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$MINFM_PACKAGE_LOG\"\n",
        );
        write_executable(&fake_bin.join("rpm"), "#!/bin/sh\nexit 0\n");
        write_executable(
            &fake_bin.join("dpkg-query"),
            "#!/bin/sh\necho 'install ok installed'\n",
        );

        Self {
            _temp: temp,
            home,
            fake_bin,
            package_log,
        }
    }

    fn add_command(&self, command: &str) {
        write_executable(&self.fake_bin.join(command), "#!/bin/sh\nexit 0\n");
    }

    fn make_package_install_fail(&self) {
        write_executable(
            &self.fake_bin.join("sudo"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$MINFM_PACKAGE_LOG\"\nexit 1\n",
        );
    }

    fn run(&self, answer: Option<&str>) -> std::process::Output {
        let installer = format!("{}/install.sh", env!("CARGO_MANIFEST_DIR"));
        let mut command = if answer.is_some() {
            let mut command = Command::new("/usr/bin/script");
            command.args([
                "-qec",
                &format!("/bin/sh '{}'", installer.replace('\'', "'\\''")),
                "/dev/null",
            ]);
            command.stdin(Stdio::piped());
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.arg(installer);
            command
        };
        command
            .env("HOME", &self.home)
            .env("PATH", &self.fake_bin)
            .env("MINFM_VERSION", "v-test")
            .env("MINFM_PACKAGE_LOG", &self.package_log)
            .env_remove("XDG_BIN_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(answer) = answer {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("{answer}\n").as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn package_commands(&self) -> String {
        fs::read_to_string(&self.package_log).unwrap_or_default()
    }
}

#[test]
fn installer_verifies_and_places_the_release_without_a_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = temp.path().join("fake-bin");
    fs::create_dir(&fake_bin).unwrap();
    let config_dir = temp.path().join(".config/minfm");
    fs::create_dir_all(&config_dir).unwrap();
    let config = config_dir.join("config.toml");
    fs::write(&config, "[icons]\ntheme = 'nerd-font'\n").unwrap();
    let install_dir = temp.path().join(".local/bin");
    fs::create_dir_all(&install_dir).unwrap();
    fs::write(install_dir.join("minfm"), b"old-minfm\n").unwrap();
    let curl = fake_bin.join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
destination=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        destination=$1
    fi
    shift
done
case "$destination" in
    *.sha256)
        hash=$(printf 'fake-minfm\n' | /usr/bin/sha256sum | /usr/bin/awk '{print $1}')
        printf '%s  minfm-linux-x86_64\n' "$hash" > "$destination"
        ;;
    *) printf 'fake-minfm\n' > "$destination" ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&curl).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&curl, permissions).unwrap();
    let fc_cache = fake_bin.join("fc-cache");
    fs::write(&fc_cache, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&fc_cache).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fc_cache, permissions).unwrap();

    let path = format!("{}:/usr/bin:/bin", fake_bin.display());
    let output = Command::new("/bin/sh")
        .arg(format!("{}/install.sh", env!("CARGO_MANIFEST_DIR")))
        .env("HOME", temp.path())
        .env("PATH", path)
        .env("MINFM_VERSION", "v-test")
        .env_remove("XDG_BIN_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(temp.path().join(".local/bin/minfm")).unwrap(),
        b"fake-minfm\n"
    );
    assert!(fs::read_dir(install_dir).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".minfm-install.")));
    assert!(temp.path().join(".config/minfm").is_dir());
    assert_eq!(
        fs::read(temp.path().join(".local/share/fonts/minfm/minfm-icons.ttf")).unwrap(),
        b"fake-minfm\n"
    );
    assert!(fs::read_dir(temp.path().join(".local/share/fonts/minfm"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".minfm-font-install.")));
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "[icons]\ntheme = 'nerd-font'\n"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Installed minfm"));
}

#[test]
fn installer_script_has_valid_posix_shell_syntax() {
    let status = Command::new("/bin/sh")
        .args(["-n", &format!("{}/install.sh", env!("CARGO_MANIFEST_DIR"))])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn installer_skips_ripgrep_prompt_when_rg_is_available() {
    let fixture = InstallerFixture::new();
    fixture.add_command("rg");

    let output = fixture.run(None);

    assert!(output.status.success());
    assert!(fixture.home.join(".local/bin/minfm").is_file());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ripgrep"));
    assert!(fixture.package_commands().is_empty());
}

#[test]
fn installer_maps_ripgrep_for_each_supported_distribution() {
    for (manager, expected) in [
        ("dnf", "dnf install -y ripgrep\n"),
        ("apt-get", "apt-get update\napt-get install -y ripgrep\n"),
        ("pacman", "pacman -S --needed ripgrep\n"),
    ] {
        let fixture = InstallerFixture::new();
        fixture.add_command(manager);

        let output = fixture.run(Some("y"));

        assert!(
            output.status.success(),
            "{manager} stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fixture.package_commands(), expected, "manager: {manager}");
        assert!(fixture.home.join(".local/bin/minfm").is_file());
    }
}

#[test]
fn declining_ripgrep_keeps_core_installation_available() {
    let fixture = InstallerFixture::new();
    fixture.add_command("dnf");

    let output = fixture.run(Some("n"));

    assert!(output.status.success());
    assert!(fixture.home.join(".local/bin/minfm").is_file());
    assert!(fixture.package_commands().is_empty());
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Filename and metadata search remain available"));
}

#[test]
fn noninteractive_install_without_ripgrep_succeeds_with_guidance() {
    let fixture = InstallerFixture::new();
    fixture.add_command("apt-get");

    let output = fixture.run(None);

    assert!(output.status.success());
    assert!(fixture.home.join(".local/bin/minfm").is_file());
    assert!(fixture.package_commands().is_empty());
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Install the ripgrep package to enable content search"));
}

#[test]
fn unsupported_package_manager_keeps_core_installation_available() {
    let fixture = InstallerFixture::new();

    let output = fixture.run(Some("y"));

    assert!(output.status.success());
    assert!(fixture.home.join(".local/bin/minfm").is_file());
    assert!(fixture.package_commands().is_empty());
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("No supported package manager found; install ripgrep manually"));
}

#[test]
fn failed_ripgrep_install_keeps_core_installation_available() {
    let fixture = InstallerFixture::new();
    fixture.add_command("dnf");
    fixture.make_package_install_fail();

    let output = fixture.run(Some("y"));

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.home.join(".local/bin/minfm").is_file());
    assert_eq!(fixture.package_commands(), "dnf install -y ripgrep\n");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Content search remains unavailable"));
}
