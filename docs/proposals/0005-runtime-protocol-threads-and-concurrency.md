# Runtime protocol, threads, and concurrency

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

Multiple frontends, streaming observations, hooks, durable threads, and child
agents need explicit ownership. This proposal separates control from
observation and telemetry, defines attachment semantics, and gives concurrent
work structured lineage and cancellation.

## Commands, events, hooks, and telemetry

The runtime exposes three planes:

- **Control:** commands, permission decisions, and installed hooks may start,
  stop, transform, or reject work.
- **Observation:** subscribers render or relay runtime state; subscriber
  failure cannot change execution.
- **Telemetry:** diagnostic spans and metrics describe execution without
  participating in it.

Hooks are sequential and explicitly awaited when ordering matters. A hook that
changes tool arguments runs before permission evaluation. Output needed for
recovery is persisted before execution so resumption does not recompute a
different action under changed extension code. A hook may narrow or block an
action but cannot grant authority beyond user policy.

Public events are typed, serializable, and secret-free. An event reporting a
durable fact is emitted only after that fact commits. Transient deltas are
explicitly live-only. Telemetry includes identifiers, counts, durations, and
status by default; prompt content, completions, tool arguments, headers, and
secrets require explicit redaction-aware opt-in.

## Client attachment

Attachment has atomic snapshot-and-stream semantics:

1. the runtime captures one consistent snapshot and begins buffering later
   events;
2. the client receives the snapshot;
3. buffered events flush in order, followed by live events.

The implementation may use buffering or durable cursors, but it cannot leave
an observation gap between the snapshot and live delivery. Reconnection
obtains a fresh snapshot rather than treating a live stream as replayable
state. Command and result types remain serializable and suitable for a remote
proxy; this does not require every pure engine function to be asynchronous.

## Proposed identities

- A **project** associates work with a directory or repository.
- A **thread** is a durable conversation lineage with a runtime-owned head.
- A **turn** is one root request operating on a thread.
- An **agent** is an execution value with a conversational provider/model,
  tools, limits, routes, a permission ceiling, and an optional parent.

One root turn may mutate a thread at a time, preventing frontends from racing
to append incompatible next states. The runtime may execute root turns for
different threads, bounded child agents owned by an admitted turn, and
read-only frontend subscriptions concurrently.

Child work carries parent and thread lineage and participates in structured
cancellation, depth, turn, token, and authority limits. No detached work may
silently outlive its owner.

Conversation entries are immutable and may identify a parent entry. Moving a
thread head or creating a branch does not rewrite or duplicate its shared
prefix. This proposal does not introduce a separate public "lane" abstraction;
that concept should wait for a demonstrated product need.

Durable operation identities and recovery are developed in
[Proposal 0004](0004-durable-operations-and-recovery.md).

## Open questions

- What is the smallest command and event protocol needed by more than one
  frontend?
- How are controlling and observing clients authenticated and distinguished?
- What cancellation guarantees cross provider and execution-backend
  boundaries?
- When, if ever, should a thread expose more than one writable head?
