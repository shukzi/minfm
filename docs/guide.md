# minfm user guide

Detailed installation and usage documentation. For a quick introduction, see
[the README](../README.md).

- [Installation](#installation)
- [Installation details](#installation-details)
- [Uninstall](#uninstall)
- [Configuration](#configuration)
- [Archiving and compression](#archiving-and-compression)
- [Search](#search)
- [Build from source](#build-from-source)
- [Safety](#safety)
- [About the project](#about-the-project)

## Installation

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
available. Updates preserve your configuration and trash.

To install a specific release, use the version-pinned command in the
[README installation section](../README.md#install).

## Installation details

The installer downloads the static Linux x86-64 binary and its SHA-256 checksum.
It verifies the checksum before installing anything.

Before running it, ensure `curl`, `sha256sum` (from `coreutils`), and `fc-cache`
(from `fontconfig`) are installed. The installer stops if one is missing.

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
The bundled font omits selected optional brand symbols and custom glyph names;
all built-in icons and the remaining symbol shapes and spacing are preserved.

The basic file manager needs only a Linux terminal and the installed binary.
The single install command above handles the rest: it installs minfm, checks the
tools used by its optional content-search, device, Samba, partition, and
filesystem features, and shows what is missing. It then asks whether it may
install the corresponding packages with your distribution's package manager.
Optional packages are installed only after your confirmation.

The installer supports Fedora, Debian/Ubuntu, and Arch Linux. These package
names are the same on all three:

`fontconfig`, `xdg-utils`, `util-linux`, `udisks2`, `cryptsetup`,
`smartmontools`, `hdparm`, `parted`, `sudo`, `coreutils`, `e2fsprogs`,
`ntfs-3g`, `dosfstools`, `xfsprogs`, `btrfs-progs`, `f2fs-tools`,
`exfatprogs`, `udftools`, and `ripgrep`.

Only the Samba package names differ:

- Fedora and Arch Linux: `gvfs-smb`, `libsecret`
- Debian and Ubuntu: `gvfs-backends`, `libsecret-tools`

You may decline any package prompt and continue using the basic file manager.
Without `ripgrep`, filename and metadata search remain available but content
search is disabled. If an optional tool in `M` is unavailable, minfm explains
what packages are required when you open it.

Connected Samba shares use the desktop's authenticated GIO/GVfs session and
behave like other browser directories. Copy, cut, and paste can transfer files
between a share and local storage. When copy verification is enabled (the
default), it checks file content and directory structure; metadata that one
filesystem cannot represent, such as extended attributes on many Samba shares,
is skipped with a warning. Cross-filesystem moves are always verified before
minfm attempts to move the source to trash. If the source filesystem cannot do
that, minfm keeps the source and identifies the verified destination instead of
deleting data permanently.

## Uninstall

Remove the minfm binary and its configuration data:

```sh
rm -f ~/.local/bin/minfm
rm -rf ~/.config/minfm
rm -rf ~/.local/share/fonts/minfm
fc-cache -f
```

This deliberately leaves the shared desktop trash untouched.

## Configuration

The configuration file is:

```text
~/.config/minfm/config.toml
```

A missing configuration uses safe built-in defaults. To customize it, copy the
documented [example configuration](../config.example.toml) to that location and
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
[config.example.toml](../config.example.toml) for the complete, deliberately small
set. Overrides must be printable and one to three terminal cells wide so browser
columns remain aligned.

For example:

```toml
[hotkeys]
tools = "F2"
devices = "F4"
archive = "F5"
```

Files open with the Linux default application. To use another editor, set it in
the configuration:

```toml
[open]
opener = "xdg-open"
editor = "nvim"
```

## Archiving and compression

Press `z` on selected files or directories to create an archive. Press `z` on
a `.tar`, `.tar.gz`, `.tgz`, or `.zip` file to inspect its contents or extract
it into a chosen directory. Mark multiple entries first to place them together
in one archive.

Archive support is built into minfm and does not require additional system
packages. Creation and extraction run in the background, can be cancelled, and
use temporary output that is cleaned after failure or cancellation. Extraction
refuses unsafe paths, escaping links, duplicate archive paths, special device
entries, and existing destination items rather than silently overwriting data.

ZIP support covers stored and standard Deflate compression. TAR archives retain
safe Unix permissions, timestamps, and links; ZIP archives retain permissions
and safe symbolic links where the format provides them. ZIP extraction retains
read, write, and execute permissions while stripping SUID, SGID, and sticky bits
from files and directories.

## Search

Press `/` for a quick search in the current directory. Type a name and press
Enter, or press `F` while the query is still empty to expand the same form to
advanced search. Pressing `F` from the browser opens advanced search for the
entire filesystem. The advanced form can instead search only the current
directory or recurse from the current directory.

Name matching supports smart, glob, and regular-expression modes. Smart mode
is case-insensitive and ranks exact, prefix, substring, and fuzzy matches.
Name, content, type, size, and modified-time constraints use AND semantics.
Multiple file types may be selected. Minimum/maximum size and modified-after/
modified-before bounds are inclusive; size filters exclude directories because
directory entry sizes do not represent their contents.

Search respects hidden-file and ignore rules by default. The advanced traversal
option can include ignored and hidden entries. Searches never follow symbolic
links. Content search is optional and invokes `rg` (ripgrep) in bounded batches;
there is no slower built-in content-search fallback. If `rg` is unavailable,
only submissions with a content query are disabled—filename and metadata search
continue to work.

Results arrive while traversal is running. Press Esc to cancel and return to
the previous browser or result view. Choose a 1,000, 5,000, or 10,000 result
cap in the advanced form; reaching it marks the result set as truncated. From
results you can open directories or files, mark multiple entries, copy, cut,
rename, trash, archive, edit text files, and view information. Paste targets
the current browser directory, so return to a directory before pasting copied
or cut results. Creating files/directories, opening the trash or tools, changing
sort/view settings, and refreshing are intentionally unavailable in results.
Operations revalidate result paths and safely remove entries that disappeared
or changed type since the search.

## Build from source

Install the current stable Rust toolchain if needed:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Download, build, and run minfm:

```sh
git clone git@github.com:shukzi/minfm.git
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
- New filesystems keep their default permissions, matching KDE Partition
  Manager. For Linux filesystems that store Unix permissions, the format screen
  can explicitly allow everyone to read and write at the filesystem's top
  level.
- FAT32, exFAT, and NTFS portability is governed by filesystem support and the
  mounting computer's options; minfm does not write Linux ownership into them.
- Copy and restore operations avoid overwriting existing destinations and clean
  up incomplete output after failure or cancellation.
- Archive extraction validates paths and links before installation, stages
  output privately, and refuses to overwrite existing destination items.
- Keep independent backups of important data. Safety checks reduce risk but do
  not make partitioning, formatting, or erasure reversible.

## About the project

minfm began with a simple goal: create a file manager that is simple, intuitive,
and easy to configure, while keeping configuration optional.

The file managers I tried handled browsing well, but left an important part of
working with a Linux system outside the application. None offered the default,
integrated way I wanted to mount drives or safely unlock encrypted volumes from
the same terminal interface.

minfm brings those needs together in one TUI without requiring Lua or plugins.
Bug reports and feature suggestions are welcome through the
[issue forms](https://github.com/shukzi/minfm/issues/new/choose). The maintainer
handles all official changes and pull requests; external pull requests are not
accepted. See [CONTRIBUTING.md](../CONTRIBUTING.md) for the feedback policy.
minfm is designed to be approachable for everyday file work and configurable
for people who want to tune their workflow.

Terminal applications are complex enough on their own. A graphical application
often adds a large dependency stack, background services, and another interface
to learn and maintain, while a terminal application can remain comparatively
lightweight. Together with other well-designed terminal tools, TUIs can cover
an entire workflow without requiring a user to leave the terminal. minfm
follows that principle by keeping common file and storage work close at hand
without turning the core into a larger system than it needs to be.

The device manager and network-share tools are optional. In most cases, their
packages are already present, so the installer will not prompt you to install
them. If you prefer a minimal file manager and those packages are not present,
you can leave them out and continue using minfm for file management.
