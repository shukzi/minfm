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

- **Navigation:** File and directory navigation with arrow-key and Vim-style controls
- **Views:** Tree view by default with a toggleable table and details view
- **Search:** Search the current directory or across the filesystem
- **Files:** Create, rename, cut, copy, paste, and open files with a preferred text editor or default application
- **Selection:** Multi-entry selection
- **Display:** File size, permissions, and modification time
- **Sorting:** Sort by name, extension, size, type, permissions, or modification time
- **Trash:** Recoverable trash, second-precise timestamps, and permanent deletion from the trash
- **Devices:** Integrated in-TUI disk manager with LUKS unlock, mount, unmount, lock, and safe eject
- **Network:** Discover, add, open, remember, and safely disconnect Samba shares
- **Apps:** Open built-in tools with `M`, including the device and partition managers
- **Partitions:** Inspect block topology, format common filesystems, create GPT or
  MBR partition tables, and create partitions in available space
- **Updates:** Background startup checks with checksum-verified installation
- **Configuration:** Invalid-configuration detection with safe-operation blocking

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

The installer downloads into a temporary directory, verifies the checksum,
installs the binary to `~/.local/bin/minfm`, and creates `~/.config/minfm/` if
needed. It does not modify the source directory or overwrite an existing
`~/.config/minfm/config.toml` file.

The basic file manager needs only a Linux terminal and the installed binary.
Additional features use:

```text
Function         Fedora                 Debian/Ubuntu          Arch
Device manager   util-linux, udisks2,   util-linux, udisks2,   util-linux, udisks2,
                 cryptsetup             cryptsetup             cryptsetup
Samba shares     gvfs-smb, libsecret    gvfs-backends,         gvfs-smb, libsecret
                                        libsecret-tools
Partitions       parted, util-linux,    parted, util-linux,    parted, util-linux,
                 sudo                   sudo                   sudo
NVMe erase       nvme-cli               nvme-cli               nvme-cli
Filesystems      e2fsprogs, xfsprogs,   e2fsprogs, xfsprogs,   e2fsprogs, xfsprogs,
                 btrfs-progs,           btrfs-progs,           btrfs-progs,
                 dosfstools, exfatprogs dosfstools, exfatprogs dosfstools, exfatprogs
```

The installer checks these tools and asks before offering to install missing
packages using Fedora's, Debian/Ubuntu's, or Arch's package manager.

The partition manager always supports inspection through `lsblk`. Write actions
use trusted root-owned system tools. When minfm is not already running as root,
the administrator password is requested through a masked TUI prompt and passed
directly to `sudo` through standard input. Install only the helpers needed for
the filesystems you use:

```text
Action                    Commands/packages
Partition tables/layout   parted, util-linux (wipefs, sfdisk)
Rotational HDD wiping     coreutils (shred)
NVMe controller erasure   nvme-cli
Authorization             sudo
ext4                      e2fsprogs
XFS                       xfsprogs
Btrfs                     btrfs-progs
FAT32                     dosfstools
exFAT                     exfatprogs
Swap                      util-linux
```

Press `M`, choose **Partition manager**, select a device, and press `Enter` or
`a` for context-sensitive actions. Unavailable actions explain which safety
condition or target type prevents them. Confirmation defaults to **No**; choose
**Yes** explicitly before an operation can begin. New partitions use the
largest available free region by default; replace `max` with a size such as
`20GiB` or `50%` to create a smaller partition.

When a disk contains more than one free area, creation first shows every free
region in disk order. Choose a region, then keep `max` or enter a smaller size.
Existing unmounted ext4 partitions also offer **Resize**: enter the desired
final size or `max`. Growing consumes adjacent free space; shrinking reduces
the filesystem before moving the partition boundary. Filesystems without a
safe offline resize path, including XFS and LUKS containers, remain blocked.

Formatting uses a filesystem chooser for ext4, XFS, Btrfs, FAT32, exFAT, and
swap, with a short compatibility description for each choice. A filesystem
label can be entered on the following screen or left blank. Formatting first
removes old filesystem and encryption signatures, then writes the selected
filesystem so stale LUKS information does not remain visible.

The selected-device panel shows a compact summary of the path, type, size,
filesystem, label, mount state, UUID, free space, and safety status. The
technical shortcut hints remain visible in the global bar at the bottom while
Apps and partition views are open.

Disk rows own layout operations: create a partition in free space or reset the
disk to Empty, GPT, or MBR. Empty leaves the disk without a partition table;
GPT is the modern default, while MBR is available for legacy compatibility.
Partition rows own resizing, deletion, filesystem formatting and checks,
filesystem labels, GPT names, partition type IDs, and common flags. Disk rows
also allow a partition-table backup to a new file that minfm will never
overwrite.

Media-specific erasure is deliberately separate from ordinary formatting.
**Wipe HDD** appears only when Linux reports a rotational disk and offers 1, 3,
or 7 overwrite passes followed by zeros. It is hidden for SSD, flash, and NVMe
media. **Erase NVMe** asks the controller for its Sanitize capabilities and
prefers Block Erase, then Crypto Erase, then Overwrite, and monitors the raw
Sanitize status log until the controller reports completion. It never bypasses
the tool's busy-device check, and it is blocked when the controller exposes
another namespace that was not selected.
Replacing a table erases signatures from the old partitions before creating the
new GPT or MBR table, preventing stale filesystem or LUKS signatures from
reappearing when a partition is recreated at the same offset. Protected system
storage displays its actions as blocked with an explicit reason.
After a disk reset, minfm explicitly asks the kernel to reload its partition
map, waits for udev, and verifies that old partition rows are gone before it
reports success.

The installer and in-app updater require `curl` and `sha256sum`, which are
normally already available on Linux systems. Update checks run in the
background. minfm asks before downloading and installing a newer release.

## Uninstall

Remove the installed binary:

```sh
rm -f ~/.local/bin/minfm
```

To also remove minfm's configuration and remembered Samba credentials:

```sh
secret-tool clear application minfm 2>/dev/null || true; rm -f ~/.config/minfm/config.toml ~/.config/minfm/network-shares.toml
```

The trash is not removed by uninstalling minfm.

## Install a specific version

For a reproducible installation, pin the installer and release assets to the
same published version:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/v0.2.0/install.sh | MINFM_VERSION=v0.2.0 sh
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

Files open with the Linux default application. To use another editor, set it in
the configuration:

```toml
[open]
opener = "xdg-open"
editor = "nvim"
```

Select a text file and press `e` to use the configured editor. Terminal editors
such as Nano and Vim use the current terminal, then return to the same position
in minfm when closed.

Press `N` to open Network Shares. Remembered passwords use the desktop's Secret
Service; passwords are never written to minfm's configuration. Remembered share
addresses and account names are stored in:

```text
~/.config/minfm/network-shares.toml
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
- Partition actions revalidate the device path and kernel major/minor identity
  immediately before execution.
- Protected system storage, read-only devices, mounted descendants, overlapping
  boundaries, and mismatched parent disks are rejected.
- ext4 resizing is check-first, uses only contiguous free space for growth, and
  reduces the filesystem before the partition when shrinking.
- Whole-disk erasure is blocked for mounted storage, active mapped descendants,
  protected system disks, read-only devices, and changed device identities.
- Keep backups of important files.

## License

MIT License.

Copyright (c) 2026 shukzi
