# Durable operations and recovery

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

A completed conversation transcript does not establish whether an interrupted
side effect ran. This proposal separates model-visible conversation from
durable operation, audit, and recovery state so restoration never guesses at
an unknown outcome.

## Proposed records

| Record kind | Purpose | Enters model context? |
|---|---|---|
| Conversation entry | User, assistant, and tool-result content with immutable identity and optional parent | Yes, subject to context assembly |
| Operation record | Accepted work, steps, invocation intents/results, suspension, cancellation, and recovery | No |
| Context record | Versioned source/view identity, selector, provenance, trust, bounds, and content hash | Only a bounded materialization |
| Audit fact | Permission decisions and other security-relevant facts | No |
| Live event | Rendering committed facts or explicitly transient in-progress state | No; not authoritative persistence |
| Telemetry | Durations, counts, status, and diagnostic correlation | No |

A root turn is a durable operation composed of one or more steps. A step holds
an assistant response and the complete batch of tool calls requested by that
response. The runtime assigns transport-safe `OperationId`, `StepId`, and
`ToolInvocationId` values rather than persisting references to live Rust
objects.

## Invocation intent and result

Before a tool effect starts, the runtime appends an invocation-intent record
containing:

- invocation and preallocated result identities;
- final effective arguments after allowed transformations;
- the permission decision and relevant scope;
- the tool's declared replay safety.

After the effect completes, the runtime appends its result using the
preallocated identity. A missing result means the outcome is unknown, not that
the effect definitely did not occur.

Broad effect class remains separate from replay behavior. A proposed minimal
contract is:

```rust
pub enum ReplaySafety {
    Safe,
    Never,
}
```

`Safe` means the exact persisted invocation may be attempted again after an
unknown outcome. It is not inferred from a `read`, `write`, `execute`,
`network`, or `external` label. `Never` is the conservative default for unknown
tools and external effects. Recovery repeats unfinished work only when both
persisted intent and the installed tool declaration say it is safe; otherwise
it records an interrupted result.

## Operation lifecycle

Operations may be accepted, running, suspended, aborting, or finished with a
completed, failed, aborted, declined, or interrupted outcome. Suspension covers
approval waits and deferred provider or focused-service work without
pretending the owning thread is idle.

Restoration reduces persisted records into state and performs no effects.
Resumption is a separate explicit command. It reconciles interrupted work,
rechecks authority where another effect may occur, and continues from a
durable boundary. Opening a session in a frontend therefore cannot send a
message, repeat a tool, or redeem a provider handle by itself.

## Initial storage direction

An initial JSONL store would promise process-crash recovery at record
boundaries. It may truncate a malformed final record from a torn append, while
malformed interior records are visible corruption. Stronger power-loss or
`fsync` guarantees must be stated and tested. A later storage backend may
change mechanics without changing operation semantics.

## Authoritative state and compute snapshots

Durable context transformations and native plans record their inputs,
instruction/invocation identities, named intermediate references, and outputs
through the same operation boundary. Recovery reconstructs them from operation
records, immutable artifacts, and versioned context references.

An interpreter, notebook, sidecar, or other compute backend may eventually
offer a best-effort heap snapshot as a convenience. Such a snapshot is never
authoritative: open files, sockets, native objects, external resources, or
environment-sensitive values may be absent or unsafe to restore. Xana remains
correct if every compute snapshot is discarded.

The permission decision preceding intent persistence is developed in
[Proposal 0003](0003-tool-authority-and-execution.md). Live protocol and thread
ownership are developed in
[Proposal 0005](0005-runtime-protocol-threads-and-concurrency.md). Context and
native-plan records are developed in
[Proposal 0008](0008-artifact-backed-context-and-native-plans.md).

## Open questions

- Which record format and migration rules are required before persistence
  ships?
- Which failures suspend an operation versus finish it?
- How are partial multi-call steps represented and resumed?
- Which pure context derivations can be recomputed rather than persisted as
  materialized output?
- What durability guarantee is practical and testable on each supported
  platform?
