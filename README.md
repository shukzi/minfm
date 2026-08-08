# minfm

[![CI](https://github.com/shukzi/minfm/actions/workflows/ci.yml/badge.svg)](https://github.com/shukzi/minfm/actions/workflows/ci.yml)

minfm is a minimal terminal file manager for Linux, written in Rust. It provides
intuitive keyboard navigation and integrated disk management (including LUKS
functionality).

It focuses on predictable file operations and guided encrypted-volume handling
without a plugin system, embedded editor, database, or background daemon.

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

## Install

The first release must be published before the binary install command below can
download a release asset. Until then, build from source with the manual steps.

The simplest installation downloads the latest checksum-verified static Linux
x86-64 release binary:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/main/install.sh | sh
```

For a reproducible install of a specific release, pin both the installer and
the release assets. This is optional; the command above always follows the
latest release:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/v0.1.0/install.sh | MINFM_VERSION=v0.1.0 sh
```

The installer installs the binary to `~/.local/bin/minfm` and creates the user
configuration directory at `~/.config/minfm`. It never replaces an existing
configuration file. Add `~/.local/bin` to `PATH` if your shell does not already
include it, then run:

```sh
minfm
```

The static binary does not include Linux desktop services. For device management
and LUKS operations, the installer detects missing `lsblk`, `udisksctl`, and
`cryptsetup` tools and asks before offering the appropriate Fedora, Debian/Ubuntu,
or Arch package command. File management works without those optional tools.

The published static release currently targets x86-64 Linux. Other architectures
can use the manual Rust build path.

## Manual build

Install a stable Rust toolchain, then run:

```sh
cargo build --release --locked
```

The binary is written to `target/release/minfm`.

For a portable static x86-64 binary on Fedora/Debian-like systems, install the
musl toolchain and build with:

```sh
# Fedora
sudo dnf install musl-gcc

# Debian/Ubuntu
sudo apt install musl-tools

rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
```

The static binary is written to
`target/x86_64-unknown-linux-musl/release/minfm`. Published releases include a
SHA-256 checksum beside the binary.

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
