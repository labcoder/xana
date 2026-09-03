# M4 graphical semantic-parity evidence

> Ticket: M4-22
>
> Status: Implementation complete; owner and cross-platform verification pending

## Delivered contract

Xana's shared semantic model and command catalog remain authoritative across
plain terminal, TUI, and Desktop. Presentation changes pixels and interaction,
not command identity, permission, effect, result, or error meaning.

| Concern | Shared authority | Desktop projection |
|---|---|---|
| Conversation, Run, content, activity, usage | `frontend::semantic` snapshots, ordered deltas, and authoritative finals | Retained GPUI views over bounded typed projections |
| Commands | `command_catalog` stable IDs, authority, confirmation, effect, result/error codes | Native menus, buttons, and searchable palette converge on one typed dispatcher |
| Configuration and durable entities | `xana::desktop` control-plane transactions | Focused controls for connections, models, credentials, Profiles, Projects, permissions, resource policy, Workbench, repair, migration, and reset |
| Advanced integrations | Existing terminal domain commands and deterministic capability facts | Status and containment view; lifecycle mutations remain in typed terminal flows |
| Artifacts and rich content | Runtime-owned immutable artifacts, disclosure policy, and capabilities | Sanitized rich rendering, reverified bounded static-raster previews, and typed fallbacks |
| External effects | Runtime-owned typed adapters | No direct process, network, provider HTTP, credential-store, or arbitrary-path authority |
| Co-running surfaces | Foreground-host discovery and Conversation controller reducer | Desktop attaches to a compatible owner, acquires only an unclaimed controller, and otherwise remains an observer |

The Desktop command palette has an explicit exposure for every command in its
catalog. Context-bound commands name the exact control that can invoke them.
Status-only advanced-integration routes stay useful and disclose that navigation
does not perform a lifecycle mutation. Unsupported content and unknown semantic
codes remain visible through bounded non-executable fallbacks.

Managed authorization pages accept only credential-free HTTPS or loopback HTTP
URLs after runtime validation. Native, managed Codex, MCP, A2A, tool, focused-
service, and external-agent activity retain their execution owner and provenance;
projection cannot relabel them as Xana-native work.

## Automated evidence

Run from the Xana repository root:

```console
cargo test --locked frontend::conformance_tests -- --nocapture
cargo test --locked -p xana-desktop -- --nocapture
pwsh ./scripts/check-desktop-dependencies.ps1
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

The cross-surface conformance suite verifies:

- command identity, authority, confirmation, effect, success, and error codes;
- observer denial on every surface;
- safe rich-content fallback without ambient actions;
- independent resource support and permission facts; and
- snapshot/delta/final convergence before presentation.

The Desktop suite verifies explicit command exposure, typed settings routes,
status-only disclosure, owner-qualified projections, URL and native-file action
validation, hostile-Markdown sanitization, resource bounds, command convergence,
and deterministic offline Workbench fixtures. The dependency gate verifies that
CLI/TUI do not resolve GPUI, the Desktop direct-dependency set is reviewed, one
coordinated GPUI source family resolves, and presentation source has no direct
process, network, provider-HTTP, or credential-store authority.

Real loopback fixtures additionally prove attached Desktop command routing and
that an incumbent terminal controller is not displaced. A paused Desktop
projection fixture proves active runtime work completes without waiting on the
renderer, then converges through deferred critical updates or snapshot resync.
Settings-manager and native-path control work runs on the GPUI background
executor rather than the foreground presentation executor.

## Intentional surface differences

- Workbench layout, native menus, windows, notifications, and inline static-
  raster previews are Desktop-only presentation capabilities.
- Terminal surfaces retain text/metadata media fallbacks and the complete M3
  advanced-integration lifecycle commands.
- Desktop currently exposes advanced integration state and containment, then
  points lifecycle mutations to those typed terminal commands. This is an
  approved M4 surface limitation, not hidden or simulated functionality.
- Platform menu, notification, accessibility, and native-dialog behavior is
  verified per platform rather than assumed from a common fixture.

## Verification still owned by M4-23 and M4-24

Cross-platform CI, load/performance/resource gates, native accessibility, live
provider workflows, visual judgment, and the complete owner walkthrough remain
open. This record does not claim those observations or authorize release.
