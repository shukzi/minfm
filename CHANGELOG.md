# Changelog

## Unreleased

### Added

- Navigation: restore the previous selector when returning to a parent
  directory.
- Search — current directory: search visible entries with `/` and filter the
  browser table.
- Search — filesystem-wide: search usable filesystem paths with `F` in a
  background worker.
- Search — filesystem-wide: cancel searches with Esc, report skipped permission
  errors, and limit results to 10,000 entries.

### Changed

- Search — filesystem-wide: display full paths in results.
- Search — filesystem-wide: open directories directly, or open a file's
  containing directory and select the file.
- UI: document both search shortcuts in the footer and help popup.
- Navigation: selector restoration uses entry paths rather than numeric
  indexes.
