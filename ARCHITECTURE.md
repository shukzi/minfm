# minfm architecture

This repository contains the performance-first architecture introduced in
minfm 0.8.0. It
preserves the reference application's features, configuration, safety rules,
and hot-path implementations while replacing the large central source files
with explicit runtime, application-workflow, rendering, search, and storage
boundaries.

## Design rules

1. Performance and safety take precedence over architectural uniformity.
2. Input and rendering must not perform blocking filesystem, archive, network,
   device, update, or subprocess work.
3. Worker queues stay bounded, obsolete work is cancelled, and generations
   reject stale results.
4. Rendering stays event-driven and limited to visible rows.
5. Paths remain byte-preserving on Linux; do not introduce lossy UTF-8 path
   conversions.
6. File and archive installation must retain staging, cleanup, verification,
   and no-replace behavior.
7. Storage operations must retain protected-device checks, fresh identity
   validation, trusted helper resolution, private secret transport, explicit
   confirmation, and postcondition verification.
8. Avoid general frameworks, broad trait layers, cloned root state, unbounded
   channels, and allocations introduced only to make modules look uniform.

## Runtime boundary

- `src/main.rs` is intentionally tiny and calls the library entry point.
- `src/lib.rs` composes the crate and exposes `run()`.
- `src/cli.rs` owns command-line parsing and compatibility.
- `src/runtime.rs` owns terminal setup/restoration, the event-driven loop,
  background polling, animation cadence, and terminal-editor handoff.

The runtime polls every background domain in each loop turn. The bitwise `|`
chain in `poll_background` is intentional: changing it to short-circuiting
`||` can starve later queues. Busy input polling remains 16 ms and idle polling
remains 100 ms.

## Application workflows

`src/app/mod.rs` owns shared application state and stable UI models. Behavior is
split by reason to change:

- `browser.rs`: browser/tree state transitions and navigation helpers;
- `file_flow.rs`: creation, rename, clipboard, trash, archives, and launching;
- `search_flow.rs`: search form/result transitions and result revalidation;
- `network_flow.rs`: network-share UI workflow;
- `device.rs`: contextual device and LUKS workflow;
- `partition_menu.rs`: storage action availability, menus, and authorization;
- `partition_input.rs`: storage overlays and cursor-aware form input;
- `polling.rs`: background completion/progress integration;
- `input.rs`: top-level modal and mode dispatch;
- `update_flow.rs`: update prompt workflow;
- `tests.rs`: application characterization tests.

The root `App` remains the single owner of large mutable collections. This is
deliberate: moving them into generic event payloads or view models would add
copies and synchronization without improving runtime behavior.

## Rendering

`src/ui/mod.rs` performs root layout and dispatch. Feature renderers live in:

- `browser.rs`, `search.rs`, and `storage.rs` for the largest views;
- `dialogs.rs` for prompts, errors, confirmations, progress, and reports;
- `chrome.rs` for the header, status, shortcuts, and shared layout helpers;
- `tools.rs` for the tools launcher and network/device panels;
- `tests.rs` for terminal-buffer characterization tests.

Keep formatting and metadata work outside `draw` when it can be cached. Never
render all entries merely to display the viewport.

## Search and storage

Advanced search lives in `src/search/mod.rs`, with its extensive benchmark and
safety suite isolated in `src/search/tests.rs`. Its bounded update queue,
ripgrep argument budget, process-group cleanup, cancellation, cached sort keys,
and result caps are performance and correctness contracts.

Storage is layered under `src/partition/`:

- `geometry.rs`: sizes, alignment, and free regions;
- `validate.rs`: pure policy and stale-state validation;
- `plan.rs`: explicit command plans and postcondition checks;
- `process.rs`: trusted executable and privileged process handling;
- `mod.rs`: inventory composition and operation orchestration;
- `tests.rs`: command, validation, safety, and fixture coverage.

Do not merge these layers back into UI workflow code. Destructive hardware
tests must use explicitly verified disposable media and are never part of the
ordinary test suite.

## Validation contract

Run the complete release gate before accepting structural or hot-path changes:

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

Ignored tests are benchmarks. Run the relevant benchmark against both the
reference and rewrite with identical fixtures, alternating run order and using
at least five samples. A measured regression blocks a structural change until
it is explained or removed.
