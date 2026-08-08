# minfm

<p>
  <a href="https://github.com/shukzi/minfm/actions/workflows/ci.yml"><img src="https://github.com/shukzi/minfm/actions/workflows/ci.yml/badge.svg" alt="CI status" height="20"></a>
  <a href="https://github.com/shukzi/minfm/releases/latest"><img src="https://img.shields.io/github/v/release/shukzi/minfm?label=latest%20release" alt="Latest release" height="20"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/shukzi/minfm" alt="MIT License" height="20"></a>
</p>

minfm is a minimal terminal file manager for Linux, written in Rust. It provides
intuitive keyboard navigation and integrated disk management, including LUKS
functionality.

![minfm preview](assets/preview.png)

## Features

- File and directory navigation
- Arrow-key and Vim-style navigation
- Hidden-file toggle
- Sorting by name, extension, size, type, permissions, and modification time
- File size, permissions, and modification-time display
- Multi-selection
- Cut, copy, paste, rename, and directory creation
- Copy verification
- Operation progress
- Clear errors and failed-operation summaries
- Recoverable trash
- Trash timestamps with second precision
- Restore files from trash
- Permanent deletion from trash
- Protected system paths
- Overwrite confirmations
- Invalid-configuration protection
- Encrypted-device manager
- In-TUI LUKS unlock, mount, unmount, lock, and eject operations
- Masked LUKS passphrase entry

## Install

Install the latest release:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/main/install.sh | sh
minfm
```

The installer:

- Downloads the static Linux x86-64 binary
- Verifies its SHA-256 checksum
- Installs it to `~/.local/bin/minfm`
- Creates `~/.config/minfm`
- Keeps existing configuration files
- Checks for missing LUKS tools
- Asks before installing missing packages

Basic file management does not require extra runtime tools.

The device manager requires:

```text
lsblk
findmnt
udisksctl
cryptsetup
```

It also requires a running UDisks2 service and the system's normal authorization
agent. The installer checks `lsblk`, `udisksctl`, and `cryptsetup`; `findmnt` is
provided by `util-linux` on supported distributions.

For a specific release:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/v0.1.0/install.sh | MINFM_VERSION=v0.1.0 sh
```

## Build from source

Install Rust if needed:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Download, build, and run minfm:

```sh
git clone https://github.com/shukzi/minfm.git
cd minfm
cargo build --release --locked
./target/release/minfm
```

## Configuration

The configuration file is:

```text
~/.config/minfm/config.toml
```

A missing configuration uses safe built-in defaults.

An invalid configuration opens a blocking error screen. File operations remain
disabled until the configuration is corrected and reloaded.

An example configuration is included in:

```text
config.example.toml
```

## Safety

- Delete moves items to the trash.
- Permanent deletion is only available from the trash view.
- Important system paths are protected.
- Overwrites require confirmation.
- Copy operations can verify the result.
- Failed operations report what did not complete.
- LUKS passphrases remain masked.
- System and root volumes cannot be locked or unmounted.
- Keep backups of important files.

## License

MIT License.

Copyright (c) 2026 shukzi
