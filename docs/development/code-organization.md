# Code organization

> Audience: Contributors and coding agents  
> Authority: Repository policy

Xana is one Cargo application package named `xana`. Its capability module owns
validated capability/tool identifiers and immutable snapshots; process entry,
the headless agent loop, application policy, and frontends remain separate
modules rather than speculative crates. Future frontend reuse starts at the
repository-private typed frontend command/event/snapshot seam, not by exposing
every internal Rust type. Extract a crate only after a second real consumer
proves the smallest shared contract. Modules continue to separate
responsibility, ownership, and I/O boundaries.

## Module boundaries

- Split code by responsibility, ownership, or I/O boundary rather than by a
  fixed line count.
- Treat roughly 400 production lines or 700 total lines as a review prompt,
  not an automatic failure.
- Use `feature.rs` as a facade with child modules in `feature/child.rs`; do not
  introduce new `mod.rs` files.
- Keep items private by default. Expose the smallest useful `pub(crate)`
  surface from each facade.
- Keep `main.rs` thin. Application routing belongs in `app`; Xana-owned native
  execution belongs in `native_runtime`, append-only interaction in
  `plain_terminal`, and vendor-loop adaptation in `managed_execution`.
- Keep control-plane routing in `app`; put interactive/one-shot provider,
  session, runtime, and surface construction behind `app::chat`'s small
  interface. Keep hosting, automation output, operation recovery, and session
  inspection in their focused private app children.
- Do not move configuration, environment reads, provider wire types, or
  terminal rendering into the headless agent loop.
- Keep native generation in `provider`, connection-owned catalog/selection in
  `model_catalog`, static API-key ownership in `credential`, and foreign agent
  protocols beneath `managed`.
- Keep full-screen application state and transitions in `tui/state`; rendering
  and terminal side effects stay in their existing focused TUI modules.

## Tests

Unit tests stay beside the code whose private behavior they exercise. Small
`#[cfg(test)] mod tests` blocks may remain inline. A large test block may move
to `feature/tests.rs` through `#[cfg(test)] mod tests;` without becoming an
integration test.

Top-level `tests/` targets exercise externally visible package behavior. Keep
executable smoke coverage focused. Test volume is not a defect by itself:
split when production code becomes hard to find or when tests cover several
independent responsibilities.

## Documentation and comments

- Use `//!` on architectural modules to state responsibility, invariants, and
  forbidden dependencies.
- Use `///` for caller-visible contracts, error conditions, and security or
  resource guarantees.
- Use `//` for rationale, ordering constraints, and platform or protocol
  details. Do not narrate obvious control flow or target a comment percentage.
- Follow the [documentation maintenance policy](../README.md#keeping-documentation-accurate)
  when a change affects Architecture or User Documentation.

## Formatting and toolchain

Rustfmt's default Rust 2024 style is the formatting authority. Clippy's default
lint groups run with warnings denied; additional pedantic or restriction lints
are selected individually rather than enabled wholesale. The checked-in
toolchain keeps Rust, rustfmt, and Clippy aligned across developer machines and
CI. Repository text files use explicit Git line-ending rules so Windows and
macOS produce stable diffs.

The required local and CI gate is:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```
