# Desktop development

> Audience: Contributors  
> Authority: Repository policy

Xana Desktop lives in `crates/xana-desktop` inside this repository and Cargo
workspace. It links the matching Xana runtime directly; installing the `xana`
CLI is neither required nor consulted.

## Run the walking skeleton

Use the root package to create or repair configuration, then launch Desktop
from the same checkout:

```bash
cargo run --locked -- setup
cargo run --locked -p xana-desktop
```

The initial M4 slice supports native conversational connections and displays a
real Conversation plus Activity projection. A missing configuration, invalid
workspace, protocol mismatch, or unavailable runtime exits nonzero with a
stable semantic error. Managed Codex and the complete Workbench remain later
M4 work; the walking skeleton rejects managed execution before starting a
vendor process.

## Required checks

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
pwsh ./scripts/check-desktop-dependencies.ps1
```

The supported source-build targets are Windows x64, Linux x64 glibc, macOS
ARM64, and macOS Intel. The primary CI matrix covers the first three; a bounded
Intel macOS job compiles and tests the Desktop package.

## GPUI rules

- Call `gpui_ai::init` exactly once and wrap each first-level window view in one
  `gpui_component::Root`.
- Keep application state, stable IDs, requests, clocks, and progressive
  lifecycle in Xana. Components render controlled snapshots and emit intent.
- Retain GPUI entities and subscriptions; do not recreate them during render.
- Drain only bounded runtime updates per frame and never block the GPUI thread
  on provider, tool, filesystem, or shutdown work.
- Use `gpui-ai` directly for AI-native presentation and `gpui-component` for
  ordinary controls. Do not wrap every component or copy upstream source.
- A reusable missing primitive gets a minimal reproduction and upstream
  `gpui-ai` issue/contribution. Consuming it requires an explicit reviewed pin
  update.

## Troubleshooting

- `configuration_unavailable`: run `cargo run --locked -- setup` with the same
  `XANA_HOME` environment.
- `workspace_unavailable`: launch from an existing accessible directory.
- `host_busy`: another Xana frontend owns the workspace root turn. Let it
  finish or interrupt it; attach-or-own behavior is completed later in M4.
- `protocol_mismatch`: rebuild the whole workspace from one checkout and do not
  mix binaries or lockfiles.
- Window closes immediately: run from a terminal and read the redacted startup
  error on stderr. Logs and durable runtime state remain under the configured
  Xana paths; Desktop does not log credentials or artifact bytes.
