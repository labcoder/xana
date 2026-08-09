# Sessions

> Audience: People using, resuming, inspecting, or backing up Xana sessions.

Xana persists every native-provider chat as one append-only session. Bare
`xana` and `xana --plain` select the latest compatible inactive session or
create a new one. One-shot without a continuation option always creates a new
session. Interactive plain chat prints its UUID, file path, and exact resume
command. Resume an explicit id or select the latest compatible session:

```text
xana --resume SESSION_ID
xana --continue
```

`--continue` scans a bounded set of native session files and selects only the
latest valid session with the same canonical workspace. Corrupt and unrelated
sessions are not candidates. `--resume` remains exact. Resume restores the committed
conversation head, context metadata, operation states, and permission audits,
but opening performs no provider call, tool execution, project-file refresh,
or unfinished-operation replay. If the current canonical directory differs
from the stored workspace, Xana rejects resume rather than crossing workspace
identity.

Managed Codex owns its thread history. Xana does not duplicate that history in
native session JSONL, and `--resume` is rejected for a Codex selection. Xana
does retain a bounded catalog of opaque Codex thread ids for each connection
and canonical workspace, plus which one is selected. Interactive managed chat
resumes that saved handle; managed
one-shot starts a new thread unless `--continue` requests the saved compatible
handle. `/clear` deselects the current handle and starts a new thread while
retaining the old opaque id; model or reasoning changes retain the selection.

## Workspace host ownership

```text
xana session list
xana session select-managed codex THREAD_ID
```

The process-owned workspace host uses one canonical workspace identity and can
list multiple native sessions and retained managed handles. It permits one
active root turn across Xana processes in that workspace; Phase 4 children
remain bounded beneath that root. The OS file lock is authoritative. A small
descriptor records a random host id, PID, and conversation only for diagnosis;
stale metadata never authorizes Xana to signal or kill that PID.

When another root is active, a normal plain launch can create an inactive
native conversation for drafting, but submitting another root is rejected.
Exact resume and `--continue` fail rather than guessing. Wait for or cancel the
controlling terminal, or attach once an explicit foreground server is running.
Closing the embedded owner cancels its runtime-owned operation and children;
there is no daemon or retained background work yet.

Conversation ownership is not a filesystem or worktree lock. Multiple
conversations may reference the same workspace, which is useful outside code,
but parallel code edits can conflict. Prefer separate Git worktrees when work
may overlap; Xana does not create them automatically.

## Storage

Sessions and artifacts use Xana's durable data category:

```text
data/
  sessions/<session-uuid>.jsonl
  sessions/<session-uuid>.jsonl.lock
  managed-threads/<blake3-route-key>.json
  managed-threads/<blake3-route-key>.lock
  workspace-hosts/<blake3-workspace-key>.json
  workspace-hosts/<blake3-workspace-key>.lock
  artifacts/<64-character-blake3-hex>
```

With `XANA_HOME`, `data/` is beneath that absolute root. Otherwise the platform
data directory described in [Configuration](configuration.md) applies.

A managed-thread document contains only a format version, connection id,
canonical workspace, a bounded list of opaque thread ids and identity versions,
and the selected id. It contains no transcript, model context, tool state,
credential, or token. Its companion lock permits
one Xana writer for the same managed route while allowing different
workspaces. Writes are atomic and bounded. Codex remains the authority for
whether a saved id can be resumed.

Every compact JSONL envelope has format version `1`, a unique record id, the
session id, and one typed record. Conversation entries are immutable and name
an optional parent; a separate thread-head record selects the visible branch.
`/clear` moves that head to empty and does not erase earlier records. Operation
acceptance, steps, invocation intents/results, states, permission audits,
recovery decisions, named values, artifacts, contexts, and views are distinct
record kinds and never enter model history automatically. Native child
admission, nonterminal lifecycle, and terminal report are also separate record
kinds. They retain parent/root, operation, thread, route, connection, model,
and execution-owner attribution without storing a full child transcript.

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
unfinished operation states, artifact counts and bytes, context versions,
child lineage/route/owner/connection/model/lifecycle/usage/report facts, and
whether a torn tail is repairable. It does not render conversation content.
Any child whose durable prefix ends before a terminal report is shown as an
explicit read-only `Interrupted` projection. Inspection neither appends that
projection nor replays provider or tool work.

## Crash and corruption behavior

Each append first validates that the record is a legal next state and that the
active session remains within its total record and byte limits. It then writes
one bounded JSON object plus newline, flushes the file, and incrementally
updates the in-memory projection. A rejected record leaves the file unchanged.
After any append I/O failure, that writer rejects further appends so a partial
tail cannot become interior corruption.
Xana claims process-crash recovery only at complete record boundaries. It does
not claim power-loss durability or call `fsync`.

A malformed physical tail after a valid newline-terminated prefix is a torn
append. Read-only inspection reports the truncate offset. Explicit resume
rechecks the complete file length and BLAKE3 hash, then truncates only that
verified tail before opening for append. The writer lock is held during that
recheck and repair, so recovery cannot discard a concurrent append. A complete
JSON object without its final newline is treated as an uncommitted tail.
Malformed newline-terminated interior data is visible corruption and is never
skipped.

The session's companion `.lock` file is retained and locked only while a
writer is active. It prevents a second Xana process from opening the same
session for chat or recovery at the same time. Read-only inspection remains
available. A lock is released by the operating system when its process exits;
the empty companion file itself is not evidence that Xana is running.

An unfinished intent does not prove that its effect did not happen. Normal
`--resume` restores and reports it without executing. Use the separate,
explicit workflow in [Operation recovery](operations.md) to inspect or
reconcile it.

Current load limits are 256 KiB per record, 10,000 records, and 16 MiB per
session. Artifact registrations accept at most 4 MiB. Root `AGENTS.md` input is
at most 64 KiB and its automatic view is bounded independently to 16 KiB and
1,024 estimated tokens.

## Backup expectations and limits

Stop Xana before copying a session and its referenced `artifacts/` directory.
The runtime owns one writer and rejects a second writer through the companion
lock. Keep the JSONL, its lock companion, and artifacts together; copying only
the session metadata can leave context references unreadable.

Copying `managed-threads/` does not copy a Codex conversation. A handle is
useful only while the corresponding Codex-owned thread remains available to
the same Codex account/home and workspace identity. Otherwise resume fails
visibly and `/clear` starts a new thread.

There is no session deletion, garbage collection, portable-workspace rewrite,
durable session grant, invocation
auto-replay, or database migration tool yet. Explicit conservative operation
recovery is described separately. Unknown future record versions and artifact
hash mismatches fail visibly.
