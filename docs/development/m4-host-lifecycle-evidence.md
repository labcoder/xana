# M4 host lifecycle and recovery evidence

> Scope: M4-09 Diagnostics, notification policy, shutdown, and crash recovery
> Status: implementation complete; cross-platform CI pending an authorized push

Implementation: `9e93a4f` (`feat(host): add lifecycle recovery coordination`)

## Implemented contract

`execution_host` owns one explicit lifecycle: running, draining, persisting,
closing, and stopped. Requesting shutdown atomically closes admission, returns
the exact active Run and pending-approval identities, and expires every
in-memory controller. Completion requires durable-flush and exact owned-
execution cleanup proof. Unproven cleanup is a typed failure and cannot produce
a stopped state or successful receipt. Remaining active Runs become
`interrupted`; no recovery path can manufacture completion.

Host failure, controller loss, recovery action, resource pressure, and storage
failure are bounded global notices, not Conversation messages. The Desktop
facade projects their semantic code and exact optional correlation without raw
content. The focus-aware notification planner:

- accepts only approval, question, completion, failure, controller-loss, and
  host-failure attention;
- emits only while unfocused or minimized and when the exact category is
  enabled;
- deduplicates within a bounded 256-key window; and
- uses fixed lock-screen-safe titles and bodies with no prompts, output,
  reasoning, paths, tool arguments, or secrets.

OS delivery, single-instance routing, and the last-window choice UI remain the
narrow M4-15 platform adapter. The policy and `Keep Xana open` / `Cancel and
quit` / `Return` effects are already shared and deterministic.

## Conservative startup recovery

M4-09 extends the existing M3 metadata-only Diagnostics runtime. It does not
create a second log, crash format, support bundle, or repair authority. Normal
mutable CLI and Desktop startup inspect prior unclean-exit markers and reconcile
the artifact store. Cleanup recognizes only regular `.UUID.tmp` staging files,
takes an exclusive lock before removal, preserves live locked writers, ignores
symlinks/unrelated files, and scans at most 1,024 entries. Published BLAKE3-
named artifacts are never candidates. Repeated cleanup is idempotent and makes
no provider/tool call.

Native session journals and managed opaque thread records remain their existing
authorities. Restart reconstructs hosted Conversations as idle, restores no
controller, and replays no Run or approval. Existing explicit operation
recovery remains the only path that resolves uncertain committed effects.

## Deterministic evidence

- Artifact fixtures cover abandoned cleanup, live-writer preservation,
  unrelated-file preservation, published-artifact preservation, and repeated
  reconciliation.
- Host fixtures cover stop-before-new-admission, exact active-Run plans,
  controller expiry, durable-flush rejection, owned-cleanup rejection,
  interruption rather than completion, and idempotent receipts.
- Notification fixtures cover focused suppression, minimized/unfocused
  delivery, per-category switches, metadata-only copy, exact correlation, and
  deduplication.
- Existing local-host, MCP stdio, managed-runtime, process-capture, native child,
  TUI terminal-guard, Diagnostics panic, run-marker, and state-migration suites
  retain the process ownership, bounded cleanup, crash, and terminal restoration
  proofs consumed by this coordinator.

The complete local verification matrix passed on Windows:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-targets --all-features` — 1,040 passed,
  6 ignored
- `cargo test --workspace --all-targets --no-default-features` — 1,040
  passed, 6 ignored

## Deliberate limits

M4-09 does not add a daemon, tray-only mode, launch-at-login, detached or
scheduled Runs, remote notifications, automatic replay, process-name killing,
raw memory dumps, telemetry upload, or destructive repair. Native notification
delivery and close-window presentation belong to M4-15; Workbench attention
composition belongs to M4-18.
