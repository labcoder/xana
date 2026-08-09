# Bounded child orchestration and exact task routes

> Audience: Contributors and coding agents  
> Authority: None
> Status: Implemented

## Context

Xana can run a foreground native conversation or delegate a foreground
conversation to the Codex app-server. It does not yet own child-agent
admission, supervised fan-out, child cancellation, or typed result collection.
Named profiles exist, but they are not yet complete execution templates and
there is no exact task-route layer above them.

Several broad proposals explored parts of this problem. This proposal accepts
only the bounded Phase 4 subset of:

- task routing from [Proposal 0002](0002-capability-model-and-routing.md);
- conservative child interruption and recovery from
  [Proposal 0004](0004-durable-operations-and-recovery.md);
- child handles and structured concurrency from
  [Proposal 0005](0005-runtime-protocol-threads-and-concurrency.md);
- profile and route ownership from
  [Proposal 0007](0007-state-ownership-and-configuration.md); and
- declarative child composition from
  [Proposal 0008](0008-artifact-backed-context-and-native-plans.md).

Those broader proposals remain Proposed for their unaccepted scope. At
acceptance time, this proposal defined the Phase 4 contract wherever their
sketches conflicted with it.

## Implementation

The bounded Phase 4 contract is implemented in Xana 0.4.0. Current
[Architecture](../architecture/README.md) and [User Documentation](../user/orchestration.md)
are authoritative for shipped behavior; this proposal remains as historical
rationale and scope evidence. The broader proposals named above remain
Proposed for their unaccepted scope.

## Decision

Phase 4 adds bounded, runtime-owned child orchestration over exact named task
routes. Native conversational providers and the existing managed Codex
app-server are both valid execution owners. The runtime owns admission,
lineage, authority, budgets, lifecycle, observation, cancellation, and bounded
reports; each execution owner retains its existing inner conversation and tool
semantics.

Phase 4 deliberately stops at one generation of children. It does not add a
general background-agent platform, remote client attachment, or arbitrary
model-authored programs.

## Vocabulary

- **Agent profile**: a named reusable configuration template. It selects an
  exact connection, model and model options, capability set, authority ceiling,
  and default budget. A profile is configuration, not a running agent.
- **Task route**: a stable name that resolves to exactly one agent profile.
  Missing or unavailable routes fail visibly; route resolution never silently
  falls back to another connection or model.
- **Resolved agent configuration**: the immutable, validated snapshot produced
  from a route, profile, current availability, parent authority, and explicit
  child restrictions. Running children do not observe later configuration or
  model-selection changes.
- **Child agent**: a runtime-admitted execution with a parent agent, an owning
  operation and thread, and an exact resolved configuration.
- **Agent handle**: the runtime-owned durable state and read model for an agent,
  keyed by its `AgentId`. A handle identifies and observes an execution; it is
  not the task itself and does not contain an unbounded transcript.
- **Child report**: a bounded typed terminal result containing status, summary
  or structured value, usage observations, and artifact references for larger
  output.
- **Orchestration plan**: a closed, versioned, validated description of child
  admission and collection. This name is distinct from the existing runtime
  `ContextPlan`, which selects prompt context sources.

## Identities, ownership, and lifecycle

Every root or child execution has an `AgentId`. A child also records its
`parent_agent_id`, owning `OperationId`, `ThreadId`, route, resolved execution
owner, connection, model, authority, budget, usage state, and terminal report
reference. A separate handle identifier is not introduced unless a later
requirement proves that `AgentId` cannot serve as the durable reference.

The runtime is the sole owner of child tasks. Child lifecycle states are:

1. `admitted`;
2. `queued`;
3. `running`;
4. optionally `suspended` with a typed reason; and
5. exactly one of `completed`, `failed`, `cancelled`, or `interrupted`.

Admission rejection is not a child state: no handle exists until admission is
durably committed. State transitions are appended to durable session state
before the corresponding observer event is emitted. Terminal transitions are
idempotent, and dropping a caller cannot detach a child from its runtime
supervisor.

Phase 4 restoration is conservative. A nonterminal child found after process
loss is projected as interrupted and is never replayed automatically. The
durable record and any committed artifacts remain inspectable. Retained work,
reattachment to a still-running remote child, and background continuation need
a later accepted design.

## Runtime operations

The canonical operations remain explicit:

- `spawn_agent` validates admission, starts one child under runtime ownership,
  and returns its handle snapshot;
- `spawn_many` atomically reserves a bounded set of admissions and returns the
  admitted handles or a typed rejection;
- `await_agent` waits for one terminal report until a caller-supplied bound;
- `collect_agents` collects a named set of handles under an explicit ordering,
  timeout, and partial-failure policy; and
- `cancel_agent` requests cancellation and reports the observed terminal
  outcome.

Admission and collection are distinct even when composed. A single-child
`delegate_agent` convenience may perform `spawn_agent` followed by
`await_agent` inside the runtime, avoiding an unnecessary outer model turn. Its
result keeps the admission identity and terminal report as distinct fields; a
boolean `wait` mode is not added to `spawn_agent`.

Timeout does not mean cancellation. A caller must choose whether a timed-out
wait leaves the supervised child running or requests cancellation. Phase 4 has
no detach option: a child still belongs to its runtime and budget until it is
terminal.

## Bounded orchestration plans

`OrchestrationPlan` is a native Rust data model with a versioned serialized
form. Its initial operators are the same runtime operations: spawn, await,
collect, and cancel. Plans may reference handles produced by earlier steps and
may describe a statically bounded acyclic graph. Validation occurs before the
first effect and rejects:

- cycles, recursion, or dynamically generated loops;
- unknown routes, result schemas, capabilities, or prior-step references;
- fan-out, descendant, concurrency, deadline, context, report, or artifact
  bounds above the effective budget; and
- authority that the parent cannot delegate.

The plan executor calls the same supervisor operations as direct tool and CLI
requests. It does not introduce a second scheduler. Phase 4 does not accept
general filter/reduce expressions, arbitrary code execution, an embedded
interpreter, or the broader model-facing context service proposed in 0008.

## Context handoff and reports

A child starts with a fresh conversation. Xana never copies the parent's full
transcript by default. Admission receives only:

- Xana's canonical identity and built-in guidance;
- the explicit child task;
- the resolved capability and authority snapshot;
- applicable project-instruction context;
- explicitly selected bounded context previews; and
- artifact or context references the child is allowed to materialize.

Native children own independent native histories. A managed Codex child starts
a distinct Codex thread with Xana's identity handoff and the explicit task;
Codex continues to own its inner loop, tools, history, and protocol. Xana does
not add an outer summarizer call or translate the whole parent history into
Codex messages.

Reports are returned directly to the parent/runtime. Inline report fields have
hard byte and estimated-token bounds. Larger output is committed as immutable
artifacts and represented by bounded previews and references. Collection
preserves input order by default, records every child's terminal status, and
cannot discard partial failures behind a successful aggregate.

## Authority and admission budgets

The effective child authority is the intersection of:

1. the parent's remaining delegable ceiling;
2. the selected profile's ceiling; and
3. explicit restrictions on the spawn request.

Children cannot widen permissions, capabilities, workspace scope, or execution
containment. Existing permission brokers remain the approval and audit owner.
Managed-runtime approval requests retain both child and parent correlation.
If an execution owner cannot enforce the effective restriction, that route is
unavailable rather than widened or approximated. In particular, the current
Codex app-server contract does not expose a stable zero-inner-tool mode, so an
effective `deny` policy rejects a managed Codex child route.

Admission uses one runtime ledger. Capacity is reserved atomically before a
child becomes admitted so concurrent callers cannot oversubscribe the same
budget. The initial hard limits cover:

- child depth, fixed at exactly one generation;
- fan-out and total descendants;
- concurrent running children;
- model and tool rounds;
- wall-clock deadline;
- input/context estimate;
- inline report size; and
- artifact bytes.

Total descendants and the aggregate tool/context/report/artifact reservations
are cumulative for the owning session. A terminal child releases its running
concurrency slot but does not replenish those totals; otherwise sequential
children could bypass the configured hard bounds and grow retained handles
without limit. An admission that fails before its durable record rolls back
the provisional reservation.

Token and spend observations are recorded when the execution owner exposes
them. Unknown usage is a typed state, never zero. A route that cannot provide
the pre-request control or interruptible live meter needed for a requested hard
limit is unavailable for that request; a final usage count alone is observation,
not strict enforcement. Subscription rate-limit state and token usage are
different signals and are not converted into fictional monetary cost.

## Profiles and exact task routes

Phase 4 extends named profiles into complete child execution templates. A
profile selects:

- one named connection and one advertised model;
- supported model options such as reasoning effort;
- one resolved logical capability set and tool snapshot;
- maximum model/tool rounds and default orchestration limits; and
- a permission/authority ceiling that can only narrow its parent.

Task routes map stable task names such as `planner` or `worker` to exactly one
profile. A default child route may be configured explicitly, but missing route
names and unavailable profiles produce typed errors. Automatic provider
selection, ordered fallback chains, and package activation are not accepted.

Interactive root model selection remains a frontend/session concern and may
override the default root profile for that conversation. It never mutates a
named profile and does not affect a child task route. Configuration uses
`connection` as the canonical field name. A versioned migration may read the
legacy profile field `provider`, but it rejects configurations containing both
names and writes only the canonical form. Renaming the existing top-level
serialized provider table is deferred to the structured configuration work.

Route resolution produces one immutable `ResolvedAgentConfig` before
admission. Diagnostics must name the requested route, profile, connection,
model, missing capability or credential, and execution-owner mismatch without
exposing secrets.

## Native and managed execution owners

The orchestrator depends on an internal execution-owner seam, not on transport
details. That seam covers start, event observation, cancellation, terminal
reporting, and usage observations. It may remain crate-private until a second
managed runtime proves a stable public trait.

Native children compose existing conversational providers and tool snapshots.
Managed Codex children reuse the existing app-server adapter. The Codex adapter
must correlate thread and turn identifiers to the child, map `turn/interrupt`
to cancellation, map token-usage notifications when advertised by the
negotiated protocol, preserve approvals/activity, and return a bounded report.
Unsupported protocol features remain typed unavailable states.

## Observation and inspection

Runtime events and durable records carry `AgentId`, optional parent identity,
owning operation, route, execution owner, connection, and model. The CLI can
render a child tree and inspect one handle without parsing log text. High-rate
managed activity remains bounded by a finite producer queue plus per-child
event and byte projection limits. Permission requests use a separate control
lane so observation truncation cannot hide an authority decision. Durable state
records lifecycle and report facts, not every transient delta or hidden
reasoning token.

## Verification contract

Phase 4 tests cross the public runtime command/event seam and the durable
session reducer wherever practical. They use scripted native providers,
barrier-controlled tasks, the real permission broker and artifact store, and a
fake JSON-RPC Codex child. CI requires no live credential.

The required matrix includes:

- exact route/profile resolution, legacy migration, and unavailable reasons;
- atomic admission under competing callers and every budget boundary;
- one child success, failure, timeout, cancellation, caller drop, and runtime
  shutdown;
- parallel fan-out with deterministic ordering and partial failure;
- bounded inline reports, artifact overflow, corrupt references, and usage
  unknowns;
- authority narrowing and attempted escalation;
- durable transition ordering, crash-site reduction, and no automatic replay;
- native and managed owner attribution, Codex interruption, approval
  correlation, and token-usage mapping; and
- Linux, macOS, and Windows behavior.

An owner-run Codex subscription smoke test is a Phase 4 exit check, not a CI
dependency. It must prove child activity, cancellation, report collection, and
usage visibility supported by the installed Codex version.

## Explicitly deferred

Phase 4 does not implement or imply:

- children spawning children, recursive orchestration, or depth above one;
- detached/background/retained children, inboxes, family messaging, or
  reattachment;
- runtime hosting, multi-client attachment, remote controller roles, A2A, or
  a public daemon;
- automatic capability/provider routing, fallback chains, package discovery,
  lazy sidecars, or extension installation;
- a general context query service, arbitrary filter/reduce programs, code
  execution, or a language kernel;
- hooks, multiple writable conversation heads, or automatic effect replay; or
- a public managed-runtime plugin interface before another implementation
  validates the boundary.

## Consequences

Xana gains useful delegation without making concurrency ambient or unbounded.
Exact routes keep provider, model, authority, and cost/usage attribution
inspectable. Separate handles and reports make timeout and cancellation honest,
while the in-runtime convenience and orchestration plans avoid unnecessary
model turns.

The tradeoff is a narrower first orchestration release: no recursive agent
society, background work, automatic routing, or general computation layer.
Those features require evidence and new accepted designs rather than expansion
of Phase 4 by implication.
