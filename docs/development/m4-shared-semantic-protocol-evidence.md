# M4 shared semantic protocol evidence

> Scope: implementation evidence for course ticket M4-04
> Status: Complete locally; cross-platform CI remains the repository gate

Xana now has one versioned, bounded, repository-private semantic model for
official local frontends. The work defines inert facts and deterministic
reduction; later M4 tickets own runtime producers, provider/account acquisition,
commands, and specialized presentation.

## Implemented contracts

- Frontend protocol version 4 carries a defaulted semantic snapshot and bounded
  semantic event envelope alongside the legacy migration projection.
- One sequence watermark orders legacy and semantic observations. Duplicate
  deltas are ignored, gaps require a fresh snapshot, and authoritative finals
  converge after progress streaming.
- Content covers text, Markdown source, code, tables, diffs, math, safe HTTP(S)
  links, artifact-backed resources, and bounded unknown values.
- `ResourceRefV1` contains no path or bytes. It retains declared/detected media
  type, metadata, accessibility provenance, validation, and derived lineage.
- Resource configuration preserves established image defaults, rejects zero or
  values above immutable ceilings, narrows route limits with `min`, uses checked
  turn accounting, and is frozen into the initial frontend snapshot.
- Large artifact collision checks stream through a 16 KiB BLAKE3 buffer rather
  than loading the existing body into memory.
- Attachment capability and operation-specific disclosure remain distinct;
  rendering cannot authorize provider input.
- Usage distinguishes deltas from sequenced cumulative snapshots, periods,
  context occupancy, billing/quota fields, availability, source, authority, and
  freshness. Replay and older cumulative snapshots cannot double-count.
- Owner-qualified nested activity excludes hidden chain-of-thought. Stable
  attention, approvals, execution facts, and completion receipts retain exact
  Conversation/Run identities.
- Capability availability, selection, and authorization are independent.
  Semantic codes permit localization without changing action or authority.
- The future voice seam carries only adapter identity and an idempotent request
  ID; it carries no samples, stream handles, or speech policy.

## Automated evidence

Focused tests cover:

- resource defaults, ceiling failures, route narrowing, checked per-turn
  accounting, and unknown-kind round trips;
- all rich content forms, unsafe-link rejection, ambient-path exclusion, and
  future content/event fallback;
- resource-policy propagation into an initial client snapshot;
- usage delta/cumulative/reconnect/out-of-order/reset/unavailable behavior;
- activity cycles and cross-Conversation parents, exact attention
  acknowledgement, capability distinctions, completion identity, and forbidden
  secret/authority/hidden-reasoning field names;
- snapshot replay, duplicate/gap recovery, delta-only progress, authoritative
  final convergence, pseudolocalized semantic codes, and the future voice seam;
  and
- bounded frontend observations sharing one sequence watermark.

The required repository gates are:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

## Commits

- `e2ad7d2 feat(resource): define bounded resource policy`
- `24a4360 feat(frontend): add shared semantic protocol`

## Deferred ownership

M4-04B owns live provider/account usage and capability acquisition. M4-05 and
M4-06 own command/projection integration and resource adapters. Terminal and
Desktop tickets own their renderers. This ticket does not claim media playback,
general provider upload, remote transport, speech mode, memory, or public SDK
compatibility.
