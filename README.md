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
- **Icons:** Rounded monochrome icons with focused per-icon overrides
- **Sorting:** Sort by name, extension, size, type, permissions, or modification time
- **Trash:** Recoverable trash, second-precise timestamps, and permanent deletion from the trash
- **Devices:** One contextual manager for disk formatting, partition and
  filesystem maintenance, raw images, SMART tests, ATA drive settings, LUKS,
  persistent mount/encryption options, and safe eject
- **Network:** Discover, add, open, remember, and safely disconnect Samba shares
- **Tools:** Open the built-in launcher with `M` for device management and network shares
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

To update an existing installation, run the same install command again or
accept the update prompt that appears on startup when a newer version is
available. The latest binary replaces only `~/.local/bin/minfm`; your
configuration and trash remain in place.

The installer downloads the static Linux x86-64 binary and its SHA-256 checksum.
It verifies the checksum before installing anything.

The installer writes to:

```text
~/.local/bin/minfm        installed binary
~/.config/minfm/          user configuration directory
~/.local/share/fonts/minfm/  icon font
```

The installer downloads into a temporary directory, verifies the checksum,
installs the binary to `~/.local/bin/minfm`, and creates `~/.config/minfm/` if
needed. It does not modify the source directory or overwrite an existing
`~/.config/minfm/config.toml` file.

The installer supplies the symbol font required by minfm's rounded monochrome
icons and refreshes the user's font cache. It does not change the terminal's
primary text font. Reinstalling or updating minfm preserves every icon override.

The basic file manager needs only a Linux terminal and the installed binary.
The single install command above handles the rest: it installs minfm, checks the
tools used by its optional device, Samba, partition, and filesystem features,
and shows what is missing. It then asks whether it may install the corresponding
packages with your distribution's package manager. Nothing is installed without
your confirmation.

The installer supports Fedora, Debian/Ubuntu, and Arch Linux. These package
names are the same on all three:

`fontconfig`, `xdg-utils`, `util-linux`, `udisks2`, `cryptsetup`,
`smartmontools`, `hdparm`, `parted`, `sudo`, `coreutils`, `e2fsprogs`,
`ntfs-3g`, `dosfstools`, `xfsprogs`, `btrfs-progs`, `f2fs-tools`,
`exfatprogs`, and `udftools`.

Only the Samba package names differ:

- Fedora and Arch Linux: `gvfs-smb`, `libsecret`
- Debian and Ubuntu: `gvfs-backends`, `libsecret-tools`

You may decline any package prompt and continue using the basic file manager.
Features whose helpers are unavailable will explain what they need when opened.

Open Device Manager through `M`, or directly with `m`. Actions are contextual:
unsupported or unsafe choices remain visible with a short reason. Eject first
unmounts filesystems and locks LUKS mappings belonging to that drive. Formatting
offers Ext4, NTFS, and FAT first; XFS, swap, Btrfs, F2FS, exFAT, UDF, and no
filesystem are under Other. Password protection is a LUKS2 toggle, independent
of the filesystem choice.

## Uninstall

Remove the minfm binary and its configuration data:

```sh
rm -f ~/.local/bin/minfm
rm -rf ~/.config/minfm
rm -rf ~/.local/share/fonts/minfm
fc-cache -f
```

This deliberately leaves the shared desktop trash untouched.

## Install a specific version

Pin both the installer and release asset to the same tag. Replace `v0.5.2` in
both places with the version you want:

```sh
curl -fsSL https://github.com/shukzi/minfm/raw/v0.5.2/install.sh | MINFM_VERSION=v0.5.2 sh
minfm
```

## Configuration

The configuration file is:

```text
~/.config/minfm/config.toml
```

A missing configuration uses safe built-in defaults. To customize it, copy the
documented [example configuration](config.example.toml) to that location and
edit the values you want to change.

An invalid configuration opens a blocking error screen. File operations remain
disabled until the configuration is corrected and reloaded.

Every letter, symbol, and function-key shortcut can be changed under
`[hotkeys]`. The defaults remain the shortcuts shown throughout the TUI. minfm
accepts a single character, named keys such as `Space` or `F2`, and optional
`Ctrl+` or `Alt+` modifiers. Arrow keys, Enter, and Escape remain universal so
dialogs always retain a predictable safe way to navigate, apply, or cancel.
Bindings that conflict in the same screen are rejected with a configuration
error instead of producing ambiguous behavior.

minfm uses one rounded, filled monochrome icon set for files, directories, and
device types. The top action bar uses compact text like the footer. File and
directory icons can be changed
under `[icons.overrides]`; see
[config.example.toml](config.example.toml) for the complete, deliberately small
set. Overrides must be printable and one to three terminal cells wide so browser
columns remain aligned.

For example:

```toml
[hotkeys]
tools = "F2"
devices = "F4"
```

Files open with the Linux default application. To use another editor, set it in
the configuration:

```toml
[open]
opener = "xdg-open"
editor = "nvim"
```

## Build from source

Install the current stable Rust toolchain if needed:

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

- Normal deletion moves items to the desktop trash. Permanent deletion is only
  available from the trash view.
- Destructive operations require explicit confirmation and default to **No**.
- System and root storage cannot be unmounted, locked, formatted, repartitioned,
  or erased.
- Storage operations revalidate the selected device immediately before running
  and reject read-only devices, mounted descendants, identity changes, unsafe
  boundaries, and mismatched parent disks.
- LUKS and Samba passwords remain masked and never appear in command-line
  arguments or configuration files. LUKS key changes pass the new key through
  a private named pipe that is removed immediately afterward.
- Persistent mount and encryption options are validated, staged privately, and
  atomically installed in `/etc/fstab` or `/etc/crypttab`.
- Copy and restore operations avoid overwriting existing destinations and clean
  up incomplete output after failure or cancellation.
- Keep independent backups of important data. Safety checks reduce risk but do
  not make partitioning, formatting, or erasure reversible.

## License

minfm is available under the [MIT License](LICENSE).

Copyright (c) 2026 shukzi.
