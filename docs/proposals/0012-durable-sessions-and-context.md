# Durable sessions and context

> Audience: Contributors and coding agents  
> Authority: None
> Status: Implemented

## Context

Xana currently keeps conversation, operation, permission-audit, and project
context state in memory. This proposal accepts the first durable storage
boundary without accepting invocation replay, cross-process coordination,
garbage collection, portable sessions, or the broader native-plan design in
Proposals 0004, 0006, 0007, and 0008.

## Implementation

The narrow durable-session, artifact, and context contract is implemented.
[Architecture](../architecture/README.md), [Sessions](../user/sessions.md),
and [Project context](../user/project-context.md) are authoritative for current
behavior; this proposal is historical design and scope evidence.

## Accepted record model

Each session is one bounded append-only JSONL file at
`data/sessions/<SessionId>.jsonl`. Every compact, newline-terminated envelope
has version `1`, a unique record id, the owning session id, and one typed
record. The initial kinds are:

- session creation with one thread and canonical launch workspace;
- immutable conversation entry plus separate thread-head movement;
- operation state change;
- permission audit fact;
- artifact registration;
- context-version registration;
- context-view registration; and
- named-context movement to a particular id and version.

Conversation entries may name an optional parent. Only the path from the
thread head enters model history. Operation, audit, artifact, context, and view
records never enter the conversation automatically. The runtime owns the one
open writer; cross-process writer exclusion is not claimed.

Each append writes one bounded object plus newline and flushes it. This is a
process-crash record-boundary guarantee, not a power-loss or `fsync` guarantee.
Inspection is read-only and returns a committed prefix plus an explicit repair
plan for a malformed physical tail after a valid newline-terminated prefix.
Malformed interior data is corruption. Opening for append rechecks the
inspected file length and BLAKE3 hash before applying a verified tail truncate.

Reduction is pure and ordered. It rejects invalid creation, identity,
reference, version, head, parent, and operation-transition sequences. Opening
or inspecting a session performs no provider call, tool call, context refresh,
or replay. `xana --resume SESSION_ID` is the only resume form; Xana never
guesses a latest session.

## Accepted artifact model

Artifact bytes live at `data/artifacts/<blake3-hex>`. BLAKE3 establishes byte
identity only; each logical registration has its own artifact id, media type,
byte length, and owner. Publishing uses a create-new temporary file, flush,
and non-overwriting final creation. An existing digest path is reused only
after its length and digest verify. Reads enforce a caller bound and verify
both the record and content digest. No garbage-collection or power-loss claim
is accepted.

## Accepted context model

A context record is distinct from an artifact reference. It carries context
id, monotonic version, artifact reference, kind, content hash, logical size,
provenance, trust, and owner. A context view names an immutable source id and
version, selector, selected-content hash, and independent byte and estimated
token bounds. Version 1 selectors are full bounded content, inclusive one-based
line ranges, and literal line search.

Root `AGENTS.md` is refreshed only when a new root turn is accepted. Unchanged
bytes reuse the current context version; changed bytes append exactly one new
artifact/context version. Missing live input does not erase a prior version.
Views materialize from persisted artifact bytes, so a live-file change cannot
alter an old reference. A view record is committed before its materialization
may enter a prompt, and all materialized text remains subject to the ordinary
prompt budget.

## CLI and inspection

`xana` creates a new session before accepting conversation. `xana --resume
SESSION_ID` inspects, reduces, reports repair or unfinished state, explicitly
opens the verified file for append, and continues from the reduced history.
The optional `xana session inspect SESSION_ID` command is read-only and prints
identities, counts, operation states, artifact byte counts, and context
versions without conversation content.

## Deliberate limits

This proposal does not accept invocation intents/results or operation replay;
Lesson 2.6 must cross its own proposal gate. It also excludes session deletion,
automatic latest-session selection, SQLite, embeddings, portable sessions,
multi-writer coordination, persistent permission grants, and durable compute
heaps.
