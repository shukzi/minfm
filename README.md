# minfm

`minfm` is a safety-first terminal file manager for Linux. It focuses on
predictable file operations and guided encrypted-volume handling without a
plugin system, embedded editor, database, or background daemon.

## Current scope

- Responsive file table with size, Unix permissions, and modification time
- Hidden-file toggle and name, extension, size, modified, type, and permission sorting
- Multi-selection, copy, cut, paste, rename, and directory creation
- Recoverable deletion using the Linux/Freedesktop trash layout
- Exact trash timestamps with second precision, restore, and separately
  confirmed permanent deletion from inside the trash view
- Protected system and mount-root paths
- Mandatory overwrite confirmation
- Modal input isolation: while a prompt is open, browser shortcuts cannot run
- Invalid-config lockout with reload and quit actions
- Operation progress and failed-operation summaries
- Information and help views
- Guided LUKS discovery, unlock-and-mount, mount, unmount, and lock operations
- Masked in-TUI passphrase entry through UDisks2's cryptsetup-backed encrypted-device
  interface

LUKS operations require `lsblk`, `udisksctl`, `cryptsetup`, a running UDisks2
service, and the system's normal authorization agent. The passphrase stays masked
inside the TUI, is sent to `udisksctl` through standard input, is never written to
disk or placed in command arguments, and its memory is overwritten when discarded.
The device manager discovers encrypted volumes regardless of which session unlocked
them. Devices backing system mounts are protected and never receive mount, unmount,
lock, or eject actions.

## Build

Install a stable Rust toolchain, then run:

```sh
cargo build --release
```

The binary is written to `target/release/minfm`.

## Run

```sh
cargo run
cargo run -- /path/to/open
cargo run -- --read-only /path/to/open
```

The configuration is read from `$XDG_CONFIG_HOME/minfm/config.toml`, or from
`~/.config/minfm/config.toml` when `XDG_CONFIG_HOME` is unset. A missing config
uses built-in safe defaults. An invalid config opens a blocking error screen;
file interaction remains disabled until it is corrected and reloaded.

## Safety

The default test suite creates all mutable fixtures inside temporary directories.
It does not access real block devices, system directories, mounts, or the user's
real trash. LUKS tests parse synthetic `lsblk` fixtures and never execute device
commands. No privileged or destructive integration tests are included.

Release qualification additionally uses an explicitly identified disposable USB
device. Those manual tests are destructive and must never target a system disk.
