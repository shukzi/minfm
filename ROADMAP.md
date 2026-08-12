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

## Architecture investigation

The maintainer has raised the possibility of rewriting the app. The current
source supports investigating that idea, but not yet committing to a
from-scratch rewrite:

- `app.rs` coordinates application state, input, prompts, and most background
  workflows in one very large module;
- `ui.rs` renders browser, tool, storage, network, and modal interfaces;
- `partition.rs` combines discovery, validation, command planning, privilege
  handling, execution, and verification for many storage operations;
- feature modules are tested, but several responsibilities meet through the
  central application state.

The first step is an architectural analysis of responsibility boundaries,
coupling, testability, state transitions, and the separation between
application coordination, rendering, file operations, storage logic, and
integrations. That analysis should identify safe seams for incremental
extraction and determine whether maintainability is best served by modular
refactoring, broader restructuring, a changed internal architecture, or a
major rewrite.

This investigation is about maintainability. It does not itself authorize
user-facing redesign. Any future restructuring or rewrite should preserve
minfm's established behavior, configuration compatibility, terminal-first
scope, and safety guarantees unless a separate proposal explicitly changes
them. A major implementation should be discussed and divided into reviewable,
behavior-preserving stages before work begins.

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
