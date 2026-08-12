# Repository guidance

These instructions apply to the entire public minfm repository. Keep this file
focused on project work; personal setup, credentials, private backups, and
machine-specific paths do not belong here.

## Project overview

minfm is a Linux terminal file manager written in Rust 2021. It combines file
browsing and operations with optional device, LUKS, network-share, archive, and
compression features. The supported minimum Rust version is declared in
`Cargo.toml` and pinned in the GitHub workflows.

The principal modules are:

- `app.rs` and `ui.rs`: application state, input handling, and rendering.
- `operation.rs`, `trash.rs`, and `archive.rs`: file operations, trash, and
  archive/compression behavior.
- `partition.rs`, `luks.rs`, and `safety.rs`: device management and destructive
  operation safeguards.
- `network.rs`: optional network-share integration.
- `config.rs`, `icons.rs`, and `updater.rs`: configuration, bundled icon use,
  and safe self-updates.

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

Run checks proportionate to the change. The full release-quality gate is:

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
