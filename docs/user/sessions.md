# Sessions

> Audience: People using, resuming, inspecting, or backing up Xana sessions.

Xana persists every chat as one append-only session. Bare `xana` creates a new
session and prints its UUID and file path. Resume only an explicit id:

```text
xana --resume SESSION_ID
```

Xana never chooses a latest session. Resume restores the committed
conversation head, context metadata, operation states, and permission audits,
but opening performs no provider call, tool execution, project-file refresh,
or unfinished-operation replay. If the current directory differs, Xana uses
the canonical workspace stored when the session was created and reports that
choice.

## Storage

Sessions and artifacts use Xana's durable data category:

```text
data/
  sessions/<session-uuid>.jsonl
  artifacts/<64-character-blake3-hex>
```

With `XANA_HOME`, `data/` is beneath that absolute root. Otherwise the platform
data directory described in [Configuration](configuration.md) applies.

Every compact JSONL envelope has format version `1`, a unique record id, the
session id, and one typed record. Conversation entries are immutable and name
an optional parent; a separate thread-head record selects the visible branch.
`/clear` moves that head to empty and does not erase earlier records. Operation
states, permission audits, artifacts, contexts, and views are distinct record
kinds and never enter model history automatically.

Each logical artifact has its own id, media type, byte length, and owner. Its
bytes use a shared BLAKE3 content path. Existing content is reused only after
length and digest verification; a digest proves byte equality, not origin,
trust, ownership, authorization, or safety.

## Inspection

The optional stretch command is implemented:

```text
xana session inspect SESSION_ID
```

It reads without modifying the session and prints ids, path, record count,
unfinished operation states, artifact counts and bytes, context versions, and
whether a torn tail is repairable. It does not render conversation content.

## Crash and corruption behavior

Each append writes one bounded JSON object plus newline and flushes the file.
Xana claims process-crash recovery only at complete record boundaries. It does
not claim power-loss durability or call `fsync`.

A malformed physical tail after a valid newline-terminated prefix is a torn
append. Read-only inspection reports the truncate offset. Explicit resume
rechecks the complete file length and BLAKE3 hash, then truncates only that
verified tail before opening for append. A complete JSON object without its
final newline is treated as an uncommitted tail. Malformed newline-terminated
interior data is visible corruption and is never skipped.

Current load limits are 256 KiB per record, 10,000 records, and 16 MiB per
session. Artifact registrations accept at most 4 MiB. Root `AGENTS.md` input is
at most 64 KiB and its automatic view is bounded independently to 16 KiB and
1,024 estimated tokens.

## Backup expectations and limits

Stop Xana before copying a session and its referenced `artifacts/` directory.
The runtime owns one writer, but there is no cross-process lock or multi-writer
coordination. Keep the JSONL and artifacts together; copying only the session
metadata can leave context references unreadable.

There is no session deletion, garbage collection, automatic latest-session
selection, portable-workspace rewrite, durable session grant, invocation
replay, or database migration tool yet. Unknown future record versions and
artifact hash mismatches fail visibly.
