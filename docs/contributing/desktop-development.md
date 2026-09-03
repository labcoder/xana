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
cargo run --locked -p xana-desktop -- --workspace .
```

The argument-free command exercises the icon-style read-only launcher. Pass
`--workspace .` when you want the repository to be the explicit runtime
workspace. This distinction prevents a packaged icon launch from silently
using an arbitrary inherited process directory.

The current M4 slice supports native conversational connections and managed
Codex, with real Conversation, isolated Message composer, and nested Activity
projections. It also projects the shared
command registry into native menus and one retained command palette, enforces
one Desktop process per canonical `XANA_HOME`, and waits for acknowledged
runtime shutdown before removing the last window. A missing configuration,
invalid workspace, protocol mismatch, unavailable instance, or unavailable
runtime exits nonzero with a stable semantic error. Managed commands remain
typed and bounded: Codex owns its inner loop, while Xana owns its Conversation
projection, activity, approval routing, and exact later-turn selection receipts.

Review the provider-free visual system and real pinned component states without
creating configuration:

```bash
cargo run --locked -p xana-desktop -- --catalog
```

The catalog is deterministic and intentionally separate from final Workbench
layout approval. See [Desktop visual system and component ownership](desktop-visual-system.md).

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
- Keep stable command IDs in Xana's shared catalog. Native menus, palette rows,
  buttons, and shortcuts must converge on one Desktop dispatcher; unsupported
  rows stay discoverable with a disabled reason. A status-only Desktop route
  must say that lifecycle changes remain in the typed terminal flow; never label
  navigation as management.
- Keep single-instance file locks, authenticated loopback forwarding, and
  path resolution in `xana::desktop`. GPUI consumes only the closed
  focus/navigation intent and exact Xana-owned paths.
- Use `gpui-ai` directly for AI-native presentation and `gpui-component` for
  ordinary controls. Do not wrap every component or copy upstream source.
- A reusable missing primitive gets a minimal reproduction and upstream
  `gpui-ai` issue/contribution. Consuming it requires an explicit reviewed pin
  update.
- Keep raw Xana palette values in `design_system.rs`. Feature code consumes
  semantic theme, size, and motion tokens instead of inventing local colors or
  clocks.

`check-desktop-dependencies.ps1` is also an authority gate. The Desktop crate
has a reviewed direct-dependency allowlist and may not spawn processes, open raw
network sockets, call provider HTTP clients, or access the credential store.
Add those capabilities to `xana::desktop` behind a narrow typed adapter. A
dependency bump must update the allowlist deliberately and re-run license,
security, accessibility, load, and all four supported source-build targets.

## Troubleshooting

- `configuration_unavailable`: run `cargo run --locked -- setup` with the same
  `XANA_HOME` environment.
- `workspace_unavailable`: choose an existing accessible folder or pass an
  explicit `--workspace PATH`.
- `host_busy`: another Xana frontend owns the workspace root turn. Let it
  finish or interrupt it; attach-or-own behavior is completed later in M4.
- `protocol_mismatch`: rebuild the whole workspace from one checkout and do not
  mix binaries or lockfiles.
- `instance_unavailable`: stop the unresponsive same-home Desktop process and
  retry. Do not turn the private descriptor into a general IPC endpoint.
- Window closes immediately: run from a terminal and read the redacted startup
  error on stderr. Logs and durable runtime state remain under the configured
  Xana paths; Desktop does not log credentials or artifact bytes.
