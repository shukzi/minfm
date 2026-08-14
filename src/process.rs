use std::{
    io,
    process::{Child, Command},
    thread,
    time::Duration,
};

pub(crate) fn spawn_with_retry(command: &mut Command) -> io::Result<Child> {
    const RETRY_DELAYS: [Duration; 5] = [
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_millis(40),
        Duration::from_millis(80),
    ];
    for delay in RETRY_DELAYS {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(nix::libc::ETXTBSY) => {
                thread::sleep(delay);
            }
            result => return result,
        }
    }
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write, os::unix::fs::PermissionsExt};

    #[test]
    fn retries_text_file_busy() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("temporarily-busy");
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)
            .unwrap();
        writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        writer.sync_all().unwrap();
        let mut permissions = writer.metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        writer.set_permissions(permissions).unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(writer);
        });

        let status = spawn_with_retry(&mut Command::new(executable))
            .unwrap()
            .wait()
            .unwrap();

        releaser.join().unwrap();
        assert!(status.success());
    }
}
