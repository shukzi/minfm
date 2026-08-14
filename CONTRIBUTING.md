# Contributing to minfm

Thank you for helping improve minfm. Documentation, tests, bug fixes, and
carefully scoped features are all useful contributions.

minfm is intentionally a focused, terminal-first Linux file manager. Changes
should solve a concrete user problem while preserving intuitive defaults,
optional configuration, and the safety boundaries around files, storage,
encryption, credentials, and destructive operations.

## Getting started

- Search [existing issues](https://github.com/shukzi/minfm/issues) and
  [pull requests](https://github.com/shukzi/minfm/pulls) before starting.
- Use the bug-report form for reproducible defects.
- Open a feature request before implementing a substantial feature, major UI
  change, architectural change, new dependency, or safety-sensitive behavior.
  This keeps design work from being duplicated or taken outside minfm's scope.
- A direct pull request is welcome for a small, obvious fix, isolated test,
  typo, or documentation improvement. An issue is not required first.

The usual flow is:

```text
Bug → bug report → triage → fix PR → validation → merge
Feature → feature request → scope discussion → implementation → PR → validation → merge
Small obvious improvement → focused PR
```

Use `Fixes #123` or `Closes #123` in a pull-request description when merging
the PR should close an issue.

## Development environment

Development and runtime support target Linux. CI runs on Ubuntu, while the
installer recognizes Fedora, Debian/Ubuntu, and Arch Linux package names. The
published binary is currently Linux x86-64; other Linux architectures require
a source build.

The minimum Rust version is `1.97.1`, declared in `Cargo.toml` and pinned in
CI. Install it with the formatting and linting components:

```sh
rustup toolchain install 1.97.1
rustup component add rustfmt clippy --toolchain 1.97.1
rustup default 1.97.1
```

Fork the repository on GitHub, then clone your fork over SSH and retain the
public repository as `upstream`:

```sh
git clone git@github.com:YOUR-USER/minfm.git
cd minfm
git remote add upstream git@github.com:shukzi/minfm.git
git switch -c your-focused-branch
```

Build and run from the repository:

```sh
cargo build --locked
cargo run -- .
```

For inspection-focused development, force read-only behavior with:

```sh
cargo run -- --read-only .
```

The core application builds from the Rust dependencies in `Cargo.lock`.
Runtime integrations use Linux tools described in the README and checked by
`install.sh`; missing optional helpers must not prevent basic file management.

## Repository map

- `src/main.rs`, `src/lib.rs`, `src/cli.rs`, and `src/runtime.rs` compose the
  executable, parse compatible command-line options, restore the terminal, and
  run the event-driven loop.
- `src/app/` owns shared application state and separates browser, file, search,
  network, device, partition, input, and background-completion workflows.
- `src/ui/` separates browser, search, storage, tools, dialog, and shared chrome
  rendering while keeping the root draw dispatch small.
- `src/config.rs` loads defaults, validates hotkeys and icons, and migrates
  older configuration without discarding user values.
- `src/entry.rs` and `src/browser_loader.rs` read, sort, search, and stream
  directory entries.
- `src/operation.rs`, `src/trash.rs`, and `src/safety.rs` implement guarded
  file operations, Freedesktop trash behavior, and path-safety checks.
- `src/archive.rs` creates, inspects, validates, stages, and extracts TAR,
  TAR.GZ/TGZ, and ZIP archives.
- `src/block.rs`, `src/luks.rs`, and `src/partition/` discover storage and split
  geometry, policy validation, command planning, privileged execution, and
  orchestration for device, filesystem, partition, encryption, SMART, image,
  and persistent mount/encryption operations.
- `src/network.rs` discovers and manages Samba shares while keeping credentials
  out of arguments and saved share metadata.
- `src/process.rs` centralizes bounded retries for transient Linux executable
  launch races used by external-helper integrations.
- `src/launcher.rs` opens files and terminal editors; `src/updater.rs` performs
  checksum-verified self-updates.
- `tests/installer.rs` exercises installer syntax, checksum handling, atomic
  placement, icon installation, and configuration preservation.
- `.github/workflows/` defines the CI and release contracts; `deny.toml`
  defines dependency advisory, license, duplicate, and source policy.

See `ARCHITECTURE.md` for ownership boundaries, performance invariants, and the
reason some specialized fast paths intentionally remain separate.

## Development conventions

- Format Rust with `rustfmt` and keep strict Clippy free of warnings.
- Prefer focused error messages that identify the failed action without
  exposing secrets. Existing modules use project errors or contextual
  `Result<_, String>` values at integration boundaries.
- Add regression tests near the implementation when practical. Keep
  asynchronous tests deterministic; do not depend on runner timing.
- Preserve backwards compatibility for existing configuration. Missing fields
  receive defaults in memory, and migrations must retain unrelated user values.
- Update `config.example.toml` when adding or changing public configuration.
- Update README behavior descriptions for user-visible changes. Add a
  changelog entry for release-relevant behavior, not every typo or internal
  cleanup.
- Keep dependencies necessary and narrowly featured. Explain why a new crate is
  preferable to the standard library or existing dependencies, update
  `Cargo.lock`, and satisfy `cargo deny` policy.
- Treat system packages and external runtime helpers as complete integrations.
  A change that introduces, removes, renames, or alters one must update, in the
  same pull request, runtime detection, `install.sh` prompts and graceful
  fallback behavior, package mappings for every supported distribution
  (currently Fedora, Debian/Ubuntu, and Arch), README installation and feature
  documentation, relevant installer and integration tests, and any affected CI
  or release checks. Keep these sources consistent. Optional helpers must not
  prevent unaffected core functionality when unavailable or when installation
  is declined.
- Avoid unrelated cleanup in a functional change. Architectural cleanup should
  have its own proposal and focused sequence of reviews.

## Validation

Every change requires a repository-wide impact audit. Review all affected
behavior, callers, tests, configuration, examples, README and contributor
documentation, changelog and release metadata, installer behavior, CI
workflows, and optional-helper integration. Update every affected surface in
the same pull request so behavior and documentation cannot drift apart.

All code changes, including small refactors and UI text changes, must run the
complete release-quality gate below. Documentation-only changes do not require
an unrelated binary rebuild, but they must be verified against current code,
commands, versions, links, and related documentation and run every applicable
syntax and consistency check. Focused tests are useful while developing, but
they do not replace the complete gate before a code change is merged.

The normal CI contract is:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo test --release --locked
cargo deny check advisories bans licenses sources
cargo build --release --locked
/bin/sh -n install.sh
git diff --check
```

Release-relevant Linux builds use the static target:

```sh
rustup target add x86_64-unknown-linux-musl --toolchain 1.97.1
cargo build --release --locked --target x86_64-unknown-linux-musl
```

That target needs a musl C toolchain such as Fedora's `musl-gcc` or Ubuntu's
`musl-tools`. Installer changes should run `/bin/sh -n install.sh` and the
installer integration tests. Tests marked `ignored` are isolated benchmarks or
environment-dependent integration checks; run the relevant one explicitly
when changing the path it covers.

To exercise verified copying from a mounted Samba share without modifying the
share, point the opt-in test at an existing non-sensitive file:

```sh
MINFM_SAMBA_TEST_SOURCE='/run/user/UID/gvfs/smb-share:server=SERVER,share=SHARE/path/to/file' \
  cargo test --locked operation::tests::samba_source_copy_verifies_contents_without_xattrs \
  -- --ignored --exact
```

The test reads that file and writes only to an automatically removed local
temporary directory.

## Changelog and releases

Add user-visible, release-relevant behavior to `CHANGELOG.md` under the version
being prepared. Trivial documentation corrections and internal-only test
maintenance do not need their own release note.

Release preparation synchronizes the version in `Cargo.toml` and `Cargo.lock`,
the version-pinned README example, and the matching changelog section. After a
release pull request is merged and `main` passes CI, the maintainer pushes an
annotated `vX.Y.Z` tag. The existing release workflow verifies that tag against
the package version and publishes the static binary, checksums, bundled icon
font, license, and changelog text. Contributors should not create release tags
as part of an ordinary pull request.

## Safety-sensitive development

Changes involving deletion, copying, moving, restore, archive extraction,
mounting, formatting, partitioning, filesystems, raw images, SMART or drive
settings, LUKS, `/etc/fstab`, `/etc/crypttab`, privileges, or credentials need
additional scrutiny.

Preserve the guarantees implemented by the affected workflow:

- destructive actions require explicit confirmation and default safely;
- system/root storage is protected and a selected device is revalidated
  immediately before modification;
- read-only devices, unsafe extents, identity changes, mounted descendants, and
  mismatched parent disks stop applicable operations;
- normal deletion uses trash, while permanent deletion is confined to the
  trash view;
- copy, move, restore, extraction, installation, and updates do not silently
  replace existing destinations;
- temporary or staged output is cleaned after cancellation or failure, and
  final installation uses no-replace or atomic operations where implemented;
- archive paths, links, duplicates, and special entries are validated before
  extracted items are installed;
- passphrases and network credentials remain out of command arguments, normal
  configuration, debug output, and saved share metadata;
- privileged helpers and persistent system configuration retain their existing
  validation, private staging, and least-privilege boundaries;
- newly formatted ownership-capable filesystems may be assigned to the
  invoking user, but must not become globally writable.

Use fake commands, fixtures, and temporary directories for automated tests.
Hardware tests require a disposable device and an explicit check that it is not
the system/root device. Describe residual risk and the exact manual test setup
in the pull request without publishing real identifiers or secrets.

## Pull requests

A good pull request is focused and explains:

- what changed and why;
- the user-visible impact, if any;
- the related issue or prior design discussion;
- the checks and manual scenarios run;
- configuration, compatibility, dependency, and safety implications.

Include relevant tests and documentation. Avoid mixing refactors with behavior
changes unless they cannot reasonably be separated. Maintainers may ask for a
large proposal to be split into smaller reviewable steps.

See the [roadmap](ROADMAP.md) for project direction and [TODO](TODO.md) for the
small distinction between local notes and public actionable work. Security
reports follow [SECURITY.md](SECURITY.md), not a public issue.
