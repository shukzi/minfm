# minfm changelog

## v0.5.1

### Changed

- Persistent mount and encryption options still use validation, private staging,
  and atomic replacement, but no longer create stale `.minfm-backup` files.

## v0.5.0

### Added

- Device Manager: manage disks, partitions, ordinary filesystems, and LUKS
  volumes from one contextual screen opened through `M` or its direct shortcut.
- Drive health: view SMART data, start short or extended self-tests, and change
  supported ATA standby, power-management, acoustic, and write-cache settings.
- LUKS maintenance: change passphrases through private pipes and configure
  persistent mount and encryption options with atomic system-file updates.
- Disk workflows: create GPT or MBR layouts, leave a disk without a partition
  table, optionally overwrite the full disk, and create or restore raw images.
- Formatting: offer Ext4, NTFS, and FAT first, with XFS, swap, Btrfs, F2FS,
  exFAT, UDF, and no filesystem under Other; LUKS2 is an independent password
  protection toggle.
- Partition workflows: create, resize, edit, format, delete, check, repair,
  image, mount, and unmount supported storage.
- Tree view: expand directories in place with persistent tree lines that make
  the current hierarchy clear.

### Changed

- LUKS handling: restore contextual unlock-and-mount, mount, unmount-and-lock,
  and smart eject behavior that first unmounts and locks related volumes.
- Storage safety: offer automatic preflight unmounting when an operation needs
  inactive media, while explaining the change before asking for confirmation.
- Interface: use monochrome dialogs, a high-contrast black popup surround, and
  compact text action rails in both the header and footer.
- Configuration: migrate the former `partitions` binding to `devices` without
  replacing custom shortcuts, comments, or other settings.
- Documentation: replace the application preview and document install-time
  dependency checks for Fedora, Debian/Ubuntu, and Arch Linux.

### Removed

- Controller-native NVMe erase commands and their device-specific recovery UI.

## v0.4.1

### Added

- Installation and updates: install the symbol font used by minfm's project
  icons, verify its checksum, and refresh the user font cache.

### Fixed

- Icon defaults: use the approved rounded monochrome set unconditionally instead
  of selecting the letter-style fallback when no configuration exists.

### Changed

- Icon configuration: remove theme selection, preserve per-icon overrides, and
  atomically remove obsolete `theme` entries from older configuration files.

## v0.4.0

### Added

- Browser UI: show one semantic icon per entry in tree and table views, using a
  font-independent Unicode theme by default and an optional Nerd Font theme.
- Icon configuration: allow focused per-icon overrides for the file, directory,
  device, and header actions minfm actually renders, with display-width
  validation that preserves alignment.
- Nerd Font icons: use rounded, filled monochrome glyphs so the optional theme
  is visibly distinct from the Unicode fallback.
- Footer: flatten browser shortcuts into one horizontally aligned shortcut rail
  without section headers or divider columns, and wrap it to at most two rows
  when the terminal is narrow.
- Header actions: expose Trash, Information, Devices, Partitions, and the
  active sort mode in the path bar with their configured shortcuts.
- Header iconography: use a dedicated action icon set in the top-right rail so
  those controls do not reuse browser entry symbols.
- NVMe erasure: list every controller-reported Sanitize and secure Format
  method, identify controller-wide versus namespace-only scope, and let the
  user choose the method before confirmation.
- NVMe recovery: offer Exit Sanitize Failure Mode only when the controller's
  status log reports a failed Sanitize operation.

### Changed

- Browser footer: remove the solid gray background and use spaced monochrome
  shortcut keys and labels without repeating actions now shown in the header.
- Configuration: rename the built-in launcher shortcut from `apps` to `tools`,
  migrate existing configuration files atomically without changing other
  custom values or comments, and keep `apps` as a compatibility alias.
- Installation and updates: require no icon font package, never modify terminal
  fonts, and preserve the user's icon theme and overrides with the rest of the
  configuration.
- Installer reliability: check the default `xdg-open` integration and stage
  replacement binaries beside the installed executable before the final atomic
  rename, while leaving existing configuration untouched.
- Partition access: add a direct configurable `partitions` shortcut, defaulting
  to `P`, and include it in browser-context duplicate-hotkey validation.
- NVMe safety: revalidate the selected erase capability immediately before
  execution, retain the multi-namespace block for controller-wide Sanitize,
  and keep secure Format scoped to the selected namespace without `--force`.

## v0.3.0

### Added

- Network shares: open the dedicated Samba manager with `N`, discover available
  and already-mounted shares, add a share address manually, open connected
  shares, and safely disconnect them.
- Samba credentials: connect anonymously or with an account, use remembered
  credentials when available, and fall back to a session-only connection when
  Secret Service support is unavailable.
- Remembered shares: forget a saved share and its associated Secret Service
  credential from the network manager.
- Apps: open a built-in app launcher with `M` while preserving the existing
  direct `m` shortcut for encrypted devices.
- Partition manager: inspect physical disks, partitions, mapped devices,
  filesystems, labels, identifiers, mountpoints, partition tables, flags, and
  protected/read-only state from a background-refreshed TUI.
- Partition actions: format common filesystems, create GPT or MBR tables, and
  create partitions in available space through a focused initial action menu.
- Partition resizing: safely grow or shrink unmounted ext4 partitions by final
  size, with filesystem-first shrinking and partition-first growth.
- Free-space selection: choose any aligned free region on an existing
  partitioned disk before accepting the default maximum or a custom size.
- Partition tools: delete partitions, edit filesystem labels, run read-only
  checks, rename GPT partitions, change exact partition type IDs, set common
  flags, and save non-overwriting `sfdisk` table backups.
- Media-aware erasure: offer 1/3/7-pass overwrite plus a final zero pass only
  for rotational HDDs, and controller-native NVMe Sanitize preferring an
  advertised Block Erase, Crypto Erase, or Overwrite capability in that order.
- Configurable hotkeys: expose every letter, symbol, and function-key shortcut
  under `[hotkeys]`, retain the established defaults, render configured values
  throughout the TUI, and reject duplicates within each active screen.

### Changed

- Responsiveness: show mounted and remembered shares immediately while remote
  Samba discovery continues in a background worker.
- UI: show `N Shares` in the browser footer and document all network-manager
  actions in the Help popup and contextual manager footer.
- Installer: detect distribution-specific Samba and Secret Service packages on
  Fedora, Debian/Ubuntu, and Arch, then ask before installing anything.
- Installer: detect partition-layout, filesystem, HDD-wipe, NVMe, and
  authentication helpers, offer the correct packages on supported
  distributions, and verify isolated noninteractive installation in tests.
- Device discovery: share strict `lsblk` pair parsing and system-storage
  protection between the encrypted-device and partition managers.
- Partition UI: use responsive columns and dialogs, concise action wording,
  neutral confirmation buttons, and a customizable `max` default partition
  size.
- Partition details: show a compact device summary with UUID and a concise
  status instead of low-level identifiers and repeated metadata.
- Context shortcuts: keep Apps and partition-manager controls visible in the
  global bottom shortcut bar, including while their panels are open.
- Apps launcher: expand its popup and table columns with the terminal width up
  to a readable maximum.
- Formatting: replace free-form filesystem syntax with a chooser for common
  formats followed by an optional label screen, and clear old signatures before
  writing the chosen filesystem so stale LUKS metadata is removed.
- Disk layout: replace free-form table input with explicit Empty, GPT, and MBR
  choices; Empty removes partition/filesystem signatures without creating a
  new table.
- Whole-disk workflow: distinguish **Create partitions** from **Use whole
  disk**, prioritize the partitioned layout, and guide users to partition
  creation after GPT/MBR initialization.
- Disk actions: keep partition creation and partition-table reset on whole-disk
  rows while keeping filesystem formatting on partition rows.
- Action visibility: render selected and blocked action text with sufficient
  contrast and show an explicit blocked reason for protected storage.
- Error reporting: show partition-operation failures in a prominent wrapping
  dialog with the action, target, elapsed time, complete reason, and an explicit
  return to the partition manager instead of truncating the error in the status
  bar.

### Security

- Credentials: keep passwords out of command arguments, URIs, logs, and TOML;
  send them through private process input, redact debug output, and wipe owned
  password buffers when dropped.
- Process handling: bound network command output, enforce operation timeouts,
  and terminate stalled command process groups without blocking the TUI.
- Saved-share metadata: write only the non-secret address and account fields
  atomically with mode `0600`.
- Partition safety: use an explicit **No**/**Yes** confirmation that defaults to
  **No**, revalidate path and kernel major/minor identity before execution, and
  reject protected, read-only, mounted, changed, overlapping, unaligned, or
  mismatched-parent targets.
- Partition authentication: collect the administrator password in a masked TUI
  prompt, validate it through trusted `sudo` standard input, revalidate the
  target afterward, pass it through standard input to every privileged helper
  instead of relying on host-specific timestamp reuse, and invalidate the
  temporary sudo timestamp on completion.
- Table replacement: erase signatures from each old partition and the whole
  disk before creating the new table so stale LUKS/filesystem metadata cannot
  reappear in a newly created partition.
- Disk reset synchronization: explicitly reload the kernel partition map, wait
  for udev, and reject completion if an old partition row is still present.
- Result verification: confirm the requested table, empty-disk state, or
  filesystem through a fresh block-device inventory before reporting success.
- Whole-disk erasure: reject active mapped descendants and NVMe controllers
  with another unselected namespace; never pass `--force` to NVMe Sanitize.
- Privileged helpers: invoke commands without a shell and allow elevation only
  for canonical root-owned executables that are not group- or world-writable.

## v0.2.0

### Added

- Browser loading: stream large directories into the interface from one bounded
  background worker, then apply a final sorted snapshot.
- Views: use a single-pane expandable tree as the default file browser and
  toggle the existing table and details view with `v`.
- Tree navigation: expand and descend with Right/`l`, collapse and return with
  Left/`h`, and toggle directories or open files with Enter.

### Changed

- Responsiveness: move directory loading, metadata collection, sorting, and
  automatic device discovery off the UI thread.
- Navigation: cancel obsolete directory work, retain only the newest pending
  request, reject stale worker results, and preserve selector movement made
  while entries are still streaming.
- Performance: on the 20,000-entry fixture, UI-thread directory dispatch takes
  44 µs instead of 43.718 ms, with the first batch available in 741 µs and the
  final sorted snapshot completed in 49.594 ms in the background.
- Performance: UI-thread device-refresh dispatch takes 26 µs instead of 15.283
  ms, with discovery completed in 15.797 ms in the background.
- UI: replace the crowded shortcut footer with aligned navigation, file-operation,
  and view/device groups.
- Editors: restore the exact nested tree entry and expanded branches after a
  terminal editor exits.
- UI: document the active view toggle in the browser footer, Help popup, and
  Application information popup.
- Safety: identify expandable directories by entry type so tree traversal never
  follows directory symlinks.

## v0.1.4

### Added

- File creation: create a new empty file with `n` using cursor-aware filename
  editing, exclusive creation, normal Linux umask permissions, and no overwrite.

### Changed

- Device manager: show the active device-operation phase together with total
  and phase elapsed time, measured with monotonic clocks.
- Device manager: include the total duration in successful completion notices
  and failed-operation messages, and show a non-cancelling warning after 30
  seconds without forcing or interrupting the operation.
- Updater: display both installed and latest versions with the same `v` prefix.
- File opening: launch default applications asynchronously and keep successful
  opens silent so the browser remains responsive and unchanged.
- File opening: show `e Edit` only for recognized text files and ignore the
  editor shortcut for directories, images, and other non-text entries.
- Editors: run Nano, Vim, Neovim, Helix, Micro, Kakoune, and similar configured
  terminal editors in minfm's current terminal, then restore the same directory,
  selector, and multi-selection state after the editor exits.
- Error reporting: show application launch failures in a consistent in-TUI
  popup, including while correcting an invalid configuration.
- Paths: pass filenames to external applications without lossy text conversion,
  preserving valid Linux filenames that contain non-UTF-8 bytes.
- Configuration: use `xdg-open` as the default opener and editor, while keeping
  `$EDITOR` compatible for existing configurations.
- Runtime: keep graphical file opening asynchronous while using a controlled
  same-terminal handoff for terminal editors, with no new dependencies.
- Release workflow: validate tags against both current and older Cargo package
  identifier formats before publishing assets.

## v0.1.3

### Changed

- Performance: render only visible browser, search-result, and trash rows, and
  redraw only when state changes or an operation indicator is animated.
- Rendering: reduce median frame time in a 20,000-entry directory from
  22.625 ms to 0.299 ms, a 98.7% reduction and 75.7× speedup.
- Idle resource usage: reduce five-second CPU time from 1.033 seconds to
  0.045 seconds—about 20.5% to 0.9% of one CPU core—and peak resident memory
  from 54.9 MiB to 12.1 MiB.
- Navigation: reduce CPU time for 100 selector movements from 2.601 seconds to
  0.080 seconds and peak resident memory from 54.9 MiB to 12.1 MiB.
- Directory loading: use directory-entry metadata and cached sort keys,
  reducing median load-and-sort time in a randomized 20,000-entry directory
  from 44.384 ms to 31.136 ms, a 29.8% reduction.
- Search: avoid constructing complete paths for nonmatching files and use an
  allocation-free fast path for common case-insensitive matches, reducing
  median traversal time across 25,250 files from 9.593 ms to 7.770 ms, a
  19.0% reduction.
- File operations: calculate recursive operation sizes once and reuse them,
  reducing median preflight time across 25,250 files from 88.006 ms to
  44.240 ms, a 49.7% reduction.
- Resource limits: bound search and file-operation update queues and process
  updates in batches so fast workers cannot grow memory without limit or
  monopolize the UI loop.
- Safety: preserve symlink identification without following links during the
  optimized directory metadata read.
- Version display: show the embedded application version in the Help and
  Application information views, and report it with `minfm --version`, so all
  version displays stay synchronized with each release.
- Binary size: the stripped static binary increases from 1,851,880 bytes to
  1,911,784 bytes, a 3.2% increase, with no new runtime dependencies.

## v0.1.2

### Added

- Navigation: restore the previous selector when returning to a parent
  directory.
- Search — current directory: search visible entries with `/` and filter the
  browser table.
- Search — filesystem-wide: search usable filesystem paths with `F` in a
  background worker.
- Search — filesystem-wide: cancel searches with Esc, report skipped
  permission errors, and limit results to 10,000 entries.
- Updater: check GitHub for a newer release in the background when minfm
  starts.
- Updater: offer an in-TUI confirmation before downloading an available
  update.
- Updater: verify the release SHA-256 checksum and atomically replace only
  the installed binary.

### Changed

- Search — filesystem-wide: display full paths in results.
- Search — filesystem-wide: open directories directly, or open a file's
  containing directory and select the file.
- UI: document both search shortcuts in the footer and help popup.
- Navigation: selector restoration uses entry paths rather than numeric
  indexes.
- Updater: keep startup responsive and continue normally when the update
  check is offline, unavailable, or times out.
- Documentation: list current-directory and filesystem-wide file search as
  separate features.
- Documentation: explain the updater's `curl` and `sha256sum` requirements.
- CI: extract the Cargo package version correctly when validating release
  tags.
