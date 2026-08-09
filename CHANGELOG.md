# minfm changelog

## Unreleased

### Added

- Network shares: open the dedicated Samba manager with `N`, discover available
  and already-mounted shares, add a share address manually, open connected
  shares, and safely disconnect them.
- Samba credentials: connect anonymously or with an account, use remembered
  credentials when available, and fall back to a session-only connection when
  Secret Service support is unavailable.
- Remembered shares: forget a saved share and its associated Secret Service
  credential from the network manager.

### Changed

- Responsiveness: show mounted and remembered shares immediately while remote
  Samba discovery continues in a background worker.
- UI: show `N Shares` in the browser footer and document all network-manager
  actions in the Help popup and contextual manager footer.
- Installer: detect distribution-specific Samba and Secret Service packages on
  Fedora, Debian/Ubuntu, and Arch, then ask before installing anything.

### Security

- Credentials: keep passwords out of command arguments, URIs, logs, and TOML;
  send them through private process input, redact debug output, and wipe owned
  password buffers when dropped.
- Process handling: bound network command output, enforce operation timeouts,
  and terminate stalled command process groups without blocking the TUI.
- Saved-share metadata: write only the non-secret address and account fields
  atomically with mode `0600`.

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
