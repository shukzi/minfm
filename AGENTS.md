# Repository guidance

These instructions apply to the entire public minfm repository. Keep this file
focused on project work; personal setup, credentials, private backups, and
machine-specific paths do not belong here.

Contributor-facing setup and pull-request expectations are documented in
`CONTRIBUTING.md`; keep this file and that guide consistent.

## Project overview

minfm is a Linux terminal file manager written in Rust 2021. It combines file
browsing and operations with optional device, LUKS, network-share, archive, and
compression features. The supported minimum Rust version is declared in
`Cargo.toml` and pinned in the GitHub workflows.

The principal modules are:

- `lib.rs`, `cli.rs`, and `runtime.rs`: crate composition, command-line
  compatibility, terminal lifecycle, and the event-driven runtime.
- `app/`: shared application state plus browser, file, search, network, device,
  partition, input, and background-completion workflows.
- `ui/`: root layout plus browser, search, storage, tools, dialogs, and chrome
  rendering.
- `operation.rs`, `trash.rs`, and `archive.rs`: file operations, trash, and
  archive/compression behavior.
- `partition/`, `luks.rs`, and `safety.rs`: storage inventory, geometry, policy,
  validation, command planning, trusted execution, and destructive-operation
  safeguards.
- `network.rs`: optional network-share integration.
- `config.rs`, `icons.rs`, and `updater.rs`: configuration, bundled icon use,
  and safe self-updates.

See `ARCHITECTURE.md` before changing module boundaries or performance-sensitive
behavior.

## Safety and compatibility

- Never run destructive disk or LUKS tests against the system/root device.
- Preserve protected-device checks, device-identity revalidation, explicit
  confirmation, and least-privilege behavior around destructive operations.
- Pass command arguments directly; do not construct shell command strings from
  user-controlled paths, names, mount points, or passphrases.
- Keep secrets out of command lines and logs. Use the existing private-input or
  pipe mechanisms for passphrases and credentials.
- File replacement, copying, moving, extraction, updates, and installation must
  retain their no-unintended-overwrite and cleanup guarantees.
- Normal deletion uses the Freedesktop trash. Permanent deletion is available
  only from the trash view.
- Missing configuration fields receive defaults in memory. Do not overwrite or
  rewrite an existing user configuration merely to add defaults.
- Formatting an ownership-capable Linux filesystem may assign its root to the
  invoking user, but must not make it globally writable or alter filesystem
  housekeeping such as `lost+found`. Filesystems using mount-time identity
  mapping must retain that behavior.
- The bundled monochrome icon font and its automatic installer/update path are
  the supported UI default. Do not require a separately configured terminal
  font or theme.
- Optional system helpers must be detected cleanly. When unavailable, the UI
  should identify the required packages without breaking core file-manager
  functionality.
- Any change that introduces, removes, renames, or otherwise alters a system
  package or external runtime helper must update the complete integration in
  the same pull request: runtime detection, `install.sh` prompts and graceful
  fallback behavior, package mappings for every supported distribution,
  README installation and feature documentation, relevant installer and
  integration tests, and any affected CI or release checks. Keep these sources
  consistent, and preserve unaffected core functionality when an optional
  helper is unavailable or installation is declined.

## Working with changes

- Preserve unrelated local modifications and untracked files.
- Use a focused feature branch and a reviewed pull request for public changes.
- Keep commits intentional and stage only files belonging to the change.
- Use SSH for GitHub Git transport:

  ```sh
  git clone git@github.com:shukzi/minfm.git
  git remote set-url origin git@github.com:shukzi/minfm.git
  ssh -T git@github.com
  ```

- Do not commit credentials, keys, `.env` files, private backups, generated
  build output, or editor state.
- Keep README and changelog edits proportionate to user-visible behavior. Avoid
  unrelated rewrites in feature changes.

## Validation

Every change requires a repository-wide impact audit before completion. Review
all affected behavior, callers, tests, configuration, examples, README and
contributor documentation, changelog and release metadata, installer behavior,
CI workflows, and optional-helper integration. Update every affected surface in
the same change; do not leave stale documentation or compatibility behavior.

Code changes must pass the full release-quality gate regardless of apparent
size. Documentation-only changes must still be checked against current code,
commands, versions, links, and related documentation, and must run applicable
syntax and consistency checks. Do not treat a focused test as sufficient proof
that unrelated behavior remains intact.

The full release-quality gate is:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo test --release --locked
cargo deny check advisories bans licenses sources
cargo build --release --locked --target x86_64-unknown-linux-musl
/bin/sh -n install.sh
git diff --check
```

Tests marked `ignored` are isolated benchmarks or expensive measurements; run
the relevant ones explicitly when changing the paths they measure. Hardware
tests require a disposable device and explicit verification that it is not a
system/root device.

## Releases

- Synchronize the version in `Cargo.toml` and `Cargo.lock`, the version-pinned
  README install command, and the matching `CHANGELOG.md` section.
- Merge the release change to `main` and wait for CI to pass before tagging.
- Create an annotated `vX.Y.Z` tag on the verified `main` commit and push it
  through SSH. The release workflow publishes the static Linux binary, its
  checksum, the bundled icon font and license, and the matching changelog text.
- Verify the published assets, checksums, installer, update path, and reported
  version before considering the release complete.
