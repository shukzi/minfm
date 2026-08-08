# minfm changelog

## Unreleased

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
- Binary size: the stripped static binary increases from 1,851,880 bytes to
  1,911,048 bytes, a 3.2% increase, with no new runtime dependencies.

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
