# Roadmap

## Project direction

minfm aims to remain a focused, terminal-first Linux file manager with
intuitive defaults and optional configuration. Integrated file, archive,
storage, LUKS, and network-share workflows should solve concrete terminal
workflows without requiring a plugin ecosystem or weakening safety around data,
devices, privileges, and credentials.

The roadmap describes direction, not a release schedule. Accepted actionable
work belongs in GitHub Issues; small notes that are not ready for an issue
belong in [TODO.md](TODO.md).

## Current priorities

There is no committed public feature milestone at present. Current work should
favor reliability, clear behavior, backwards-compatible configuration, and
focused improvements justified by user problems or accepted issues.

## Architecture direction

The local rewrite established maintainable boundaries without replacing
already optimized behavior with slower generic abstractions. Runtime and CLI
ownership are separate from application workflows; application and rendering
are split by feature; advanced-search tests are isolated; and storage now has
separate geometry, validation, planning, privileged-process, and orchestration
layers.

Future architecture work should continue as small, measured changes. Preserve
configuration compatibility, terminal-first behavior, bounded background work,
visible-row rendering, cancellation, stale-result rejection, non-UTF-8 paths,
and all file/storage/credential safety guarantees. See `ARCHITECTURE.md` for the
current boundaries and validation contract.

## Future considerations

No additional feature commitments are currently recorded. New ideas should
start with the problem and workflow they address, explain why they belong in
minfm, and consider whether they add dependencies or core complexity.

## Non-goals

- Requiring configuration before minfm is useful.
- Requiring Lua, plugins, or a plugin ecosystem for expected core workflows.
- Growing into a graphical desktop application or adding background services
  without a concrete need.
- Trading destructive-operation, overwrite, credential, or system-storage
  safeguards for convenience.
- Treating a rewrite as a goal by itself or discarding working behavior before
  architectural evidence supports that decision.

## How to influence the roadmap

Open a problem-oriented feature request before substantial feature or
architecture work. Small obvious improvements can be proposed directly as a
focused pull request. See [CONTRIBUTING.md](CONTRIBUTING.md) for the development
and review workflow.
