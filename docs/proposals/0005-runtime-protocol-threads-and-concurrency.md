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

## Durable child handles

Child admission creates a durable handle before execution begins:

```rust
struct AgentHandle {
    operation_id: OperationId,
    agent_id: AgentId,
    parent_agent_id: AgentId,
    thread_id: ThreadId,
    state: AgentState,
    route: RouteRef,
    budget: ChildBudget,
}
```

The exact fields wait for operation and routing types. The runtime owns a state
machine such as:

```text
admitted -> queued -> running -> waiting -> completed | failed | cancelled
```

Retention and deletion are separate from execution state. Every transition is
durable and observable, and usage remains attributable to the handle's parent,
route, model, and budget. An unavailable exact route fails rather than silently
falling back.

One root turn may mutate a thread at a time, preventing frontends from racing
to append incompatible next states. The runtime may execute root turns for
different threads, bounded child agents owned by an admitted turn, and
read-only frontend subscriptions concurrently.

Child work carries parent and thread lineage and participates in structured
cancellation and authority inheritance. Budgets cover concurrency, fan-out,
depth, total descendants, tokens, turns, time, and spend; depth alone is not a
sufficient guardrail because a depth-one agent may still create a wide tree.
The default depth is one. No detached work may silently outlive its runtime
owner.

## Structured waiting and collection

Admission and results are different operations. The model-facing surface may
start with a synchronous convenience that admits one child, waits, and returns
a bounded report while retaining the handle internally. The complete runtime
contract provides bounded `spawn_many`, await, collect, timeout, and cancel
operations over handles.

Child output supports a typed result or bounded report plus artifact/context
references for overflow. Free-form messages and shared files remain useful for
long-lived collaboration, but they are not the only fan-in mechanism for
bounded analytical work. Collection defines ordering, partial-failure policy,
output limits, and cancellation behavior explicitly.

Retained/background collaborators, reattachment, inboxes, and family messaging
wait for a runtime host that can preserve ownership and delivery state beyond a
frontend connection.

Conversation entries are immutable and may identify a parent entry. Moving a
thread head or creating a branch does not rewrite or duplicate its shared
prefix. This proposal does not introduce a separate public "lane" abstraction;
that concept should wait for a demonstrated product need.

Durable operation identities and recovery are developed in
[Proposal 0004](0004-durable-operations-and-recovery.md). Native plan-based
fan-out and aggregation are developed in
[Proposal 0008](0008-artifact-backed-context-and-native-plans.md).

## Open questions

- What is the smallest command and event protocol needed by more than one
  frontend?
- How are controlling and observing clients authenticated and distinguished?
- What cancellation guarantees cross provider and execution-backend
  boundaries?
- Should the first model-facing API expose `AgentHandle`, or retain it behind a
  synchronous one-child convenience?
- What delivery guarantee should retained-agent inboxes provide?
- When, if ever, should a thread expose more than one writable head?
