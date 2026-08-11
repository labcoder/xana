# AGENTS.md

## Project

Xana is a small, extensible personal AI agent harness written in Rust. Keep the
implementation cross-platform and the headless agent independent of frontend,
process-global, and provider-wire concerns.

## Before changing Xana

1. Start at the [documentation index](docs/README.md).
2. Read [Architecture](docs/architecture/README.md) for implemented behavior
   and boundaries.
3. Read [Design Principles](docs/principles.md) for prescriptive constraints.
4. Check the relevant [proposal](docs/proposals/) when changing an area with
   future design work. Only a proposal marked Accepted is prescriptive.

Do not describe a proposal as implemented. Do not add future design claims to
Architecture or User Documentation.

## Required checks

Run these before submitting changes:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Source boundaries

- Provider wire formats remain private to their adapters. The rest of Xana
  uses its internal conversation model.
- `Agent` remains a headless value. It receives configuration and dependencies;
  it does not load global state, inspect environment variables, or render a
  frontend.
- `main.rs` remains the process composition root. Application routing belongs
  in `app`; append-only interaction belongs in `plain_terminal`, managed-loop
  adaptation in `managed_execution`, and full-screen presentation in `tui`.
- Use Rust path APIs and explicit platform behavior. Keep Linux, macOS, and
  Windows validation meaningful.
- Keep optional capabilities, authority, containment, and context budgets
  explicit as described by Design Principles.

## Code organization

Follow [Code organization](docs/development/code-organization.md). Split at
responsibility, ownership, and I/O boundaries rather than a hard line count;
keep items private by default and expose the smallest useful facade.

## Documentation impact

Update documentation in the same change when code alters:

- responsibilities, dependencies, invariants, data flow, or meaningful
  limitations described by Architecture;
- installation, CLI, configuration, paths, diagnostics, or visible behavior
  described by User Documentation; or
- an implemented proposal's status and resulting architectural description.

Refactors that preserve documented behavior and boundaries need no docs
change. Code and tests are evidence of implemented behavior; a disagreement
with descriptive documentation is a documentation defect. Keep this repository
self-contained: its documentation must stand on repository evidence and the
proposal and decision system rather than outside teaching or sequencing
material.
