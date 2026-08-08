# minfm changelog

## Unreleased

Changes for the next release will be recorded here.

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
