# M4 shared semantic protocol evidence

> Scope: implementation evidence for course ticket M4-04
> Status: Complete locally; cross-platform CI remains the repository gate

Xana has one versioned, bounded, repository-private semantic model for official
local frontends. M4-04 established the inert facts and deterministic reduction;
the protocol has since advanced compatibly to version 9 as the native and
managed producers, TUI, Desktop, Workbench, Espejo, and settings adapters were
implemented.

## Implemented contracts

- Frontend protocol version 9 carries the semantic snapshot and bounded event
  envelope. Versions 5 through 9 add stable semantic identities, execution and
  completion facts, Desktop controls, Espejo, and host-supervision state without
  changing the M4-04 content/usage/attention authority boundaries.
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

## Subsequent M4 ownership

M4-04B added live provider/account usage and capability acquisition. M4-05 and
M4-06 integrated commands, projections, and resource adapters; later Terminal
and Desktop tickets implemented their renderers. Those implementations consume
this contract rather than creating competing semantic models. M4 still does not
claim general media playback, general provider upload, remote transport, speech
mode, personal memory, or a public SDK compatibility promise.
