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
- Current-directory search and filesystem-wide search
- Background update checks with confirmed, checksum-verified updates
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
```

Run minfm from your terminal with:

```sh
minfm
```

To update an existing installation, run the same install command again. The
latest binary replaces only `~/.local/bin/minfm`; your configuration and trash
remain in place.

The installer downloads the static Linux x86-64 binary and its SHA-256 checksum.
It verifies the checksum before installing anything.

The installer writes to:

```text
~/.local/bin/minfm        installed binary
~/.config/minfm/          user configuration directory
```

It creates a temporary directory while downloading and removes it when finished.
It does not write to the source directory and does not replace an existing
configuration file.

The basic file manager needs only a Linux terminal and the installed binary. The
device manager uses:

```text
Command          Package       Required service
lsblk            util-linux    —
findmnt          util-linux    —
udisksctl        udisks2       UDisks2
cryptsetup       cryptsetup    authorization agent
```

The installer checks all four commands and asks before offering to install
missing packages using Fedora's, Debian/Ubuntu's, or Arch's package manager.
The installer and in-app updater require `curl` and `sha256sum`, which are
normally already available on Linux systems. Update checks run in the
background. minfm asks before downloading and installing a newer release.

## Uninstall

Remove the installed binary:

```sh
rm -f ~/.local/bin/minfm
```

To also remove minfm's configuration:

```sh
rm -f ~/.config/minfm/config.toml
```

The trash is not removed by uninstalling minfm.

## Install a specific version

For a reproducible installation, pin the installer and release assets to the
same published version:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/v0.1.1/install.sh | MINFM_VERSION=v0.1.1 sh
minfm
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
