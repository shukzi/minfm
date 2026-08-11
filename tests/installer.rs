use std::{fs, os::unix::fs::PermissionsExt, process::Command};

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
