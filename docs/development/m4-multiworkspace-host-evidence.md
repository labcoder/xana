# M4 multiworkspace host evidence

> Scope: implementation evidence for course ticket M4-07
> Status: Complete locally; cross-platform CI remains the repository gate

Xana now has a bounded application-owned execution coordinator above its
workspace-scoped native and managed owners. The implementation deliberately
does not add a daemon, automatic worktrees, remote authority, or a second agent
loop.

## Implemented contracts

- Stable Xana `ConversationId` is separate from an opaque managed provider
  thread. Legacy managed-handle documents derive deterministic identities and
  new documents persist schema version 3.
- One host admits at most eight Conversations and four simultaneous Runs.
- Opened filesystem identity groups path aliases into one collision domain.
  Independent workspaces may execute concurrently; a second write-capable Run
  in the same workspace requires an explicit acknowledgement.
- Conversation state, operation identity, activity, pending approvals,
  failures, interruption, completion, and terminal outcome remain isolated.
- A host snapshot and monotonically ordered bounded event suffix share one
  watermark. An evicted cursor receives a fresh snapshot instead of an
  unprovable replay.
- Restart reconstructs durable owners as idle and never replays Runs.
- Attach validates the destination owner before changing the current target;
  validation failure leaves the previous target attached.
- Native branching atomically publishes exact immutable active-path entries and
  lineage under a new session/Conversation. Managed branching retains an
  owner-native fork only when an adapter proves one; otherwise it records a
  fresh continuation and claims zero shared provider entries.
- Branch targets preserve the source's immutable Profile snapshot and optional
  Project membership. An ordinary failed cross-store publication removes the
  staged target; later M4 recovery work owns process-crash reconciliation
  between stores.

## Automated evidence

Focused suites cover:

- eight-Conversation admission, four simultaneous deterministic streams, and
  cross-delivery checks;
- independent-workspace concurrency and same-workspace alias collisions;
- retained and evicted event cursors;
- runtime sequence gaps and fresh-snapshot recovery;
- native and fake-managed attach success plus atomic failure preservation;
- restart without Run replay;
- exact completed, failed, declined, and interrupted receipts;
- approval, failure, and interruption isolation;
- exact native branch history/Profile lineage and source preservation;
- invalid native points without target publication;
- fake managed native forks and explicit unsupported-fork continuation; and
- managed state v1/v2 migration plus v3 identity round trips.

The required repository gates are:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

## Human and CI follow-up

No owner-only visual decision is required for this backend ticket. CI must
still exercise the standard Windows, macOS, and Linux matrix. Later M4 client
tickets own controller leases, in-process switching UI, Workbench navigation,
and the final manual visual/accessibility acceptance. Users should treat a
same-workspace collision warning as real: wait, use an explicit worktree, or
acknowledge only when overlapping writes are known to be safe.
