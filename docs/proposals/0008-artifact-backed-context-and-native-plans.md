# Artifact-backed context and native plans

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

Large working data should not be copied into every model prompt. Xana needs an
addressable context layer that lets an agent inspect, search, derive, and
materialize bounded views while preserving provenance, trust, ownership, and
token accounting.

This proposal adopts the useful operational properties associated with
Recursive Language Model research without adding an `RLM mode`, a persistent
language kernel, or a second blob store. Artifacts remain the byte/document
substrate, and the Rust runtime remains the complete composition path.

Xana implements a durable precursor: `xana-prompt-v1` applies one estimated
input budget to fixed layers, exact tool schemas, persisted root `AGENTS.md`
views, and actual history. Artifact-backed context records carry identity,
owner, version, provenance, trust, and recovery identity; full, line, and
literal-search selectors materialize bounded persisted bytes. That subset is
implemented through historical
[Proposal 0012](0012-durable-sessions-and-context.md). This proposal remains
unimplemented for model-facing context capabilities, native plans, and
optional computation.

## Proposed context references

Conceptually, a context record identifies one immutable or explicitly
versioned object backed by an artifact:

```rust
struct ContextRef {
    id: ContextId,
    version: u64,
    artifact: ArtifactRef,
    kind: ContextKind,
    content_hash: ContentHash,
    logical_size: u64,
    provenance: ProvenanceRef,
    trust: TrustClass,
    owner: PrincipalId,
}

struct ContextViewRef {
    id: ContextViewId,
    source: ContextId,
    source_version: u64,
    selector: ViewSelector,
    content_hash: ContentHash,
    budget: ContextBudget,
}
```

The broader model-facing service retains this proposed semantic contract:

- references are immutable or explicitly versioned;
- derived views record source, selector, source version, content hash, trust,
  and provenance;
- materialization is bounded independently by bytes, records, work, and
  estimated tokens;
- search and extraction return references or bounded previews rather than
  unbounded content;
- every model-visible materialization participates in the normal prompt
  budget; and
- context operations are typed runtime capabilities subject to ordinary
  authorization and audit.

The first portable operations are `metadata`, `read_range`, `search`,
`derive_view`, and `materialize`. Selectors beyond byte, line, record, and
query ranges require a separate portability decision.

## Ownership boundary

The runtime owns storage, versions, provenance, policy, and materialization.
The headless engine depends on an opaque context-service interface and receives
only bounded values. This preserves a provider-neutral loop and allows local
files, document extractors, memory stores, or remote services to implement the
same logical operations without exposing their storage details.

The artifact and document substrate is developed in
[Proposal 0006](0006-media-and-document-services.md). Record durability and
recovery are developed in
[Proposal 0004](0004-durable-operations-and-recovery.md).

## Native Rust context plans

A model may invoke context operations individually or submit a typed
`ContextPlan`. The plan is serialized data over a closed Rust enum, not source
code. An initial instruction set may contain:

- context operations: metadata, range reads, search, view derivation, and
  materialization;
- composition operations: sequence, bounded filter/reduce, child map, await,
  and collect;
- control data: input references, result schemas, concurrency ceilings, token
  and time budgets, and failure policy.

Every instruction receives durable operation and invocation identities.
Intermediate values are typed records or artifact/context references, so
recovery never depends on reconstructing a process heap. Child operations use
the handle and collection contract in
[Proposal 0005](0005-runtime-protocol-threads-and-concurrency.md).

Plan validation rejects unknown instructions, invalid references, unbounded
loops, recursion or fan-out beyond the parent ceiling, and budgets exceeding
the caller's authority. File, shell, network, secret, context, and child-agent
access remain separate capabilities. Effects cross the permission and durable
operation protocols, and cancellation propagates through the plan tree.

## Optional general computation

Xana remains fully functional without an interpreter, notebook kernel, or
sidecar. A general code-execution adapter may be considered only after
held-out evaluations show a concrete workload the native plan cannot express
well. It enters through an `ExecutionBackend`, declares its authorities and
containment honestly, consumes the same typed references, and cannot make an
opaque heap snapshot authoritative state.

Execution boundaries are developed in
[Proposal 0003](0003-tool-authority-and-execution.md).

## Evaluation gates

Compare these increments under the same model, provider settings, prompt,
tools, and total budgets:

1. normal prompt construction plus file/search tools;
2. context references and bounded views without children;
3. depth-one sequential children;
4. bounded parallel map with typed collection; and
5. the same task expressed as a native Rust context plan.

Measure task quality, provenance correctness, root and total tokens, model
calls, cache use, cost, wall time and tail latency, bytes inspected versus
materialized, child counts, cancellation, recovery, and duplicate effects.
Preserve failed trajectories and repeat stochastic configurations.

Prefer the simpler file/search path when references do not improve held-out
quality or cost. Keep recursion above depth one experimental until it beats
depth one under the same total-work budget. Keep general computation optional
until the native path demonstrates a measured gap and the backend passes
authority and recovery tests on every supported platform.

## Open questions

- Is `ContextRef` a distinct persisted record or a typed view over
  `ArtifactRef`?
- Which selectors belong in the portable contract?
- Which bounded instructions belong in the first `ContextPlan`?
- Which measured capability gap could justify an optional code backend?
- Which held-out workloads best expose context quality, duplication, and cost?

## Research basis

- [Recursive Language Models paper](https://arxiv.org/abs/2512.24601)
- [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent)
