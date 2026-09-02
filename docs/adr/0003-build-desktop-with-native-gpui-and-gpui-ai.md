# ADR 0003: Build Xana Desktop with native GPUI and gpui-ai

> Status: Accepted
> Date: 2026-09-01

## Context

Xana needs a native graphical client that preserves the same headless runtime,
permission, credential, Conversation, and recovery authority as its terminal
surfaces. Tauri offered mature browser accessibility and a direct path to a
local web client. Native GPUI offered one Rust application stack, a smaller
measured process tree and memory footprint, and direct control over a desktop
workbench, but carries pre-1.0 dependency churn and requires explicit
cross-platform accessibility, input, packaging, and component evidence.

The comparison also established that framework selection and component
selection are separate decisions. Rebuilding chat, streaming, tool, approval,
and agent-navigation behavior from raw GPUI would create unnecessary Xana code.
`gpui-ai` now provides those AI-native controlled components above
`gpui-component`, while leaving application state and work with the consumer.

Xana already contains the runtime, CLI, TUI, private frontend contract, and
local-host boundary in one repository. A sibling Desktop repository or separate
Cargo workspace would make coordinated protocol changes, tests, and dependency
isolation harder without creating a real ownership benefit.

## Decision

Xana Desktop uses native GPUI. `gpui-ai` is the default layer for AI-native
surfaces; `gpui-component` remains the default for general desktop controls and
layout; `gpui-base` is used selectively for reusable behavior; raw GPUI owns
framework and platform primitives.

The application, not the component library, owns provider/runtime requests,
durable state, tools, permissions, clocks, stable domain identifiers, and
lifecycle transitions. It projects bounded controlled snapshots into
components and handles their typed intent. Desktop initializes the stack once
with `gpui_ai::init` and places one `gpui_component::Root` at the first level of
each window.

Desktop lives in the existing Xana Git repository and Cargo workspace. The
initial production boundary is one member at `crates/xana-desktop`. It owns the
native process, windows, GPUI state, and presentation adapters, and depends on a
narrow repository-private seam from the existing `xana` library. It is not a
sibling repository, separate workspace, or public frontend SDK. Further crate
splits require a proven capability owner or independent compile/test boundary.

The consuming manifest pins `gpui-ai` to an exact Git revision and uses the
matching `gpui-component` revision from that checkout. GPUI uses the same Git
source identity as `gpui-component`; the committed lockfile pins the exact Zed
commit. CI rejects duplicate GPUI source families. Upgrades are isolated,
reviewed changes and must re-run affected Windows, macOS, Linux,
accessibility, IME/input, performance, size, launch, and component tests.

Xana does not vendor or silently fork the selected libraries. Product-specific
composition stays in Xana. A reusable missing AI component should be reduced
and contributed to `gpui-ai` when appropriate, then consumed through a reviewed
pin update. Maintaining a Xana fork requires a separate ADR and budget.

The local browser client is deferred and is not a Milestone 4 exit condition.
GPUI/WASM is not the selected browser renderer. Any later browser client needs
its own accepted accessibility, security, input, bundle, and maintenance plan
while preserving Rust-owned runtime authority.

## Consequences

- Xana gains one native Rust Desktop stack and can reuse `gpui-ai` chat,
  streaming, thinking, tool, approval, attachment, queue, and navigation
  behavior instead of rebuilding those contracts.
- Runtime/domain code remains reusable by CLI, TUI, Desktop, and future clients;
  graphical dependencies stay outside inactive CLI/TUI paths.
- The single repository and Cargo workspace make runtime/frontend changes and
  conformance tests atomic while retaining a separately buildable Desktop
  artifact.
- Exact Git/lockfile coordination is mandatory because duplicate GPUI sources
  create incompatible Rust types. Dependency updates are deliberate product
  work rather than routine floating upgrades.
- Windows x64, macOS ARM64, macOS Intel, and Linux x64 glibc remain required
  source-build targets. Real launch, accessibility, input, and packaging claims
  need target-specific evidence.
- Tauri's mature browser path is declined for Desktop. If GPUI cannot meet the
  required platform, accessibility, IME, packaging, or maintenance gates, the
  framework decision may be reconsidered with new evidence.
- Local web delivery moves out of the critical M4 path. Deferral does not grant
  remote, public, LAN, multi-user, or cloud authority.

## Related contracts

- [Proposal 0022: Local multi-surface Workbench and execution host](../proposals/0022-local-multisurface-workbench-and-execution-host.md)
- [ADR 0002: Keep third-party executable integrations out of Xana's process](0002-keep-third-party-integrations-out-of-process.md)
- [Design Principles: Keep the engine small through explicit seams](../principles.md#keep-the-engine-small-through-explicit-seams)
- [Design Principles: Give every path a lifecycle and owner](../principles.md#give-every-path-a-lifecycle-and-owner)
