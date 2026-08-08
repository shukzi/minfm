# minfm

[![CI](https://github.com/shukzi/minfm/actions/workflows/ci.yml/badge.svg)](https://github.com/shukzi/minfm/actions/workflows/ci.yml)

minfm is a minimal terminal file manager for Linux, written in Rust. It provides
intuitive keyboard navigation and integrated disk management (including LUKS
functionality).

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

The installer puts minfm in `~/.local/bin` and creates `~/.config/minfm` for
your configuration. Existing configuration is kept.

```sh
export PATH="$HOME/.local/bin:$PATH"
minfm
```

The static binary does not include Linux desktop services. For device management
and LUKS operations, the installer detects missing `lsblk`, `udisksctl`, and
`cryptsetup` tools and asks before offering the appropriate Fedora, Debian/Ubuntu,
or Arch package command. File management works without those optional tools.

The published static release currently targets x86-64 Linux. Other architectures
can use the manual Rust build path.

## Manual build

Build from source with:

```sh
git clone https://github.com/shukzi/minfm.git
cd minfm
cargo build --release --locked
./target/release/minfm
```

If Rust is not installed yet:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Run those commands before the build commands above.

For a portable static x86-64 binary, install the musl toolchain first:

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

- Delete moves files to the trash.
- Permanent deletion is only available from the trash view.
- Important system paths are protected.
- Overwrites require confirmation.
- Copy operations verify the result when enabled.
- Failed operations report what did not complete.
- LUKS passphrases stay masked, and system/root volumes cannot be locked or
  unmounted.
- Keep backups of important files. No file manager can replace a backup.
