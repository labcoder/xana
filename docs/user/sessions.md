# Conversations

> Audience: People using, resuming, inspecting, or backing up Xana Conversations.

`Conversation` is the canonical user-facing term. The older `xana session`,
`/session`, and `/sessions` spellings remain compatibility aliases; examples in
this document use `conversation`. On disk, native Conversation journals remain
stored beneath the historical `sessions/` directory.

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
`xana conversation archive` instead removes exactly one inactive local handle from
Xana's catalog. It never deletes the vendor-owned thread or Codex history and
refuses an active or locked handle.

## Workspace host ownership

```text
xana conversation list
xana conversation new
xana conversation search QUERY [--conversation ID] [--limit N] [--json]
xana conversation branch CONVERSATION_ID --at ENTRY_ID
xana conversation branch CONVERSATION_ID --at current
xana conversation select codex THREAD_ID
xana conversation archive codex THREAD_ID
```

`xana conversation new` starts the adaptive interactive frontend with a fresh
conversation using the current connection, model, profile, permissions, and
workspace. It preserves every prior session. Native Xana creates the durable
session during launch; managed Codex creates its vendor-owned thread on the
first turn. Use `/conversation new` for the same lifecycle while the full-screen
frontend is already open.

`xana conversation search` scans canonical retained native history in bounded
pages. It searches user text, assistant text, and committed tool-result output,
returns bounded excerpts, and performs no provider or tool call. Use an exact
Conversation id when more than one is retained; `/conversation search QUERY`
automatically scopes the command to the Conversation attached to the terminal.
Managed history remains owned by its runtime and is reported unavailable rather
than copied into a weaker Xana transcript.

The process-owned workspace host uses one opened filesystem identity and can
list multiple native sessions and retained managed handles. It permits one
active root turn across Xana processes in that workspace; Phase 4 children
remain bounded beneath that root. The OS file lock is authoritative. A small
descriptor records a random host id, PID, monotonic owner generation, and
conversation only for diagnosis; stale metadata never authorizes Xana to
signal or kill that PID. Symlink, junction, path-case, and Windows path-prefix
aliases share the same collision domain.

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

## Branching a Conversation

Branching preserves the source and creates a separately resumable
Conversation with the same frozen Profile and optional Project membership. To
branch a native Conversation, first inspect its bounded metadata:

```text
xana conversation inspect SESSION_ID
xana conversation branch CONVERSATION_ID --at ENTRY_ID
```

For native Conversations, `CONVERSATION_ID` currently has the same UUID text as
`SESSION_ID`. Inspection prints the active history count and at most the newest
128 immutable entry ids, oldest to newest, without printing message content.
Choose one of those ids as `ENTRY_ID`. The new journal reuses the exact
immutable entries through that point, records source lineage, and leaves the
source head and records unchanged. The receipt prints the new Conversation id
and exact resume command.

Managed history stays owned by its provider. Use the stable Xana Conversation
id shown by `xana conversation list` and the explicit current owner boundary:

```text
xana conversation branch CONVERSATION_ID --at current
```

If a managed adapter proves a native fork operation, Xana retains the returned
opaque thread under the new Conversation id. Codex does not currently expose a
fork through Xana's managed adapter, so Xana records a fresh managed
continuation instead. Its frozen Profile and lineage are preserved, but shared
history is reported as zero and the provider creates a new thread on the first
turn. Xana never copies native history into Codex or claims that a fresh
provider thread contains the source transcript.

Overlapping write-capable Runs in one canonical workspace are rejected unless
the initiating surface presents and records an explicit risk acknowledgement.
Safe choices are to wait, use a separate Git worktree, or acknowledge only
when the work cannot conflict. Different spellings, symlinks, junctions, path
case, UNC paths, and Windows extended prefixes do not create independent
collision domains for the same opened directory.

## TUI navigation

The wide TUI projects the bounded host snapshot into a session rail. Use
`/conversation` or the command palette to open the searchable Conversation picker
at any width. Every row includes text—not color alone—for active, controlled,
inactive, unread, error, observable, or unavailable state. Native rows show a
bounded title derived from retained user text after inspection, execution
owner, known connection/model facts, record count, and stored modification
time. Older native sessions do not durably retain their historical model, so
Xana says `not retained` instead of guessing. Managed rows retain connection
and opaque thread identity but no transcript or reliable recency.

Pressing Space on an inactive native row opens the newest page of its committed
transcript read-only. Pressing Enter attaches to an eligible inactive row and
rebuilds the execution owner around that exact Conversation. A page contains at
most 128 messages; reaching the older edge requests the preceding page and
preserves the visible scroll anchor. The session journal scan retains entry
ancestry and byte offsets, then reads only the selected messages into the
frontend. At most 512 projected messages remain in the TUI cache.

Previewing a managed row explains that Codex still owns the transcript. Preview
does not transfer controller ownership, cancel the active root, change the
model, or send a draft. Incoming completion/error state for the attached
Conversation remains visible as a text indicator. Enter or
`/conversation attach ID` may attach an eligible inactive managed handle through
its frozen Profile; Xana does not copy the provider-owned transcript.

The TUI isolates unsent text, cursor/selection, staged images, selected vision
route, and queued input by exact Conversation. An attach saves the source state
and restores only the destination state. If the source has an active Run or
queued input, or the destination is active, controlled, observable, unavailable,
missing, or owned by an incompatible execution adapter, the attach is refused
without changing the current target. Preview remains available when safe.

`/conversation view show` and `/conversation view hide` persist the default wide rail
state in a version-1 workspace/frontend preference beneath
`data/frontend/workspaces/`. The file stores only the Boolean layout choice;
active work, selection, unread/error state, transcript data, and controller
ownership are always recomputed from runtime/host truth. Medium and narrow
layouts use the picker overlay regardless of the saved rail choice.
Hidden means zero width: the panel reserves no placeholder columns. Clicking
the visible panel title hides it. View an inactive managed row and run
`/conversation archive`, or pass its exact ID with `/conversation archive ID`, to remove
the same local handle from inside the TUI.

`/conversation new` is a separate idle lifecycle action. It exits the current TUI
owner and composes a fresh native session or managed Codex thread with the
current resolved configuration; it does not erase or translate the previous
history. An active root must finish or be interrupted first. Managed Codex
creates the vendor-owned thread lazily on the new session's first turn.

Use `/espejo [global|project]` to inspect the bounded local workspace through
attention and execution state rather than transcript order. Espejo can preview a
selected Conversation with Enter but does not acknowledge unrelated attention.
See [Espejo](espejo.md).

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
  frontend/workspaces/<blake3-workspace-key>.toml
  artifacts/<64-character-blake3-hex>
```

With `XANA_HOME`, `data/` is beneath that absolute root. Otherwise the platform
data directory described in [Configuration](configuration.md) applies.

A managed-thread document contains only a format version, connection id,
canonical workspace, a bounded list of stable Xana Conversation ids, opaque
thread ids and identity versions, and the selected id. It contains no
transcript, model context, tool state,
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

Native compaction is another append-only record kind. It stores a bounded lossy
summary, predecessor, initiating operation and reason, exact compacted entry
range and digest, verbatim-tail boundary, and the conservative model/budget
plan used. It never replaces a conversation entry or moves the thread head.
Automatic compaction runs synchronously before an over-threshold request can
reach the provider; `/compact` requests it manually while idle. Cancellation
cannot leave a half-applied logical checkpoint: either the complete validated
record is appended or canonical history is unchanged. A malformed physical
tail follows the ordinary journal-repair rules below.

Each logical artifact has its own id, media type, byte length, and owner. Its
bytes use a shared BLAKE3 content path. Existing content is reused only after
length and digest verification; a digest proves byte equality, not origin,
trust, ownership, authorization, or safety.

## Inspection

The optional stretch command is implemented:

```text
xana conversation inspect SESSION_ID
```

It reads without modifying the session and prints ids, path, record count,
active history count, at most the newest 128 immutable branch-point entry ids,
unfinished operation states, artifact counts and bytes, context versions,
child lineage/route/owner/connection/model/lifecycle/usage/report facts, and
whether a torn tail is repairable. It also reports the total compaction count
and at most the newest 64 checkpoint ids, reasons, source ranges/digests,
predecessors, tail boundaries, model/connection, estimated context and input
budgets, threshold, and retained-tail target. It does not render conversation
content or the lossy checkpoint summary. Changing provider or model composes a
new runtime budget; an existing checkpoint still retains the older plan that
produced it. Managed Codex owns its context and has no native Xana checkpoint.
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
1,024 estimated tokens. Compaction summaries default to 16 KiB and remain
subject to both their configured safe ceiling and the per-record limit.

## Backup expectations and limits

Stop Xana before copying a session and its referenced `artifacts/` directory.
The runtime owns one writer and rejects a second writer through the companion
lock. Keep the JSONL, its lock companion, and artifacts together; copying only
the session metadata can leave context references unreadable.

Copying `managed-threads/` does not copy a Codex conversation. A handle is
useful only while the corresponding Codex-owned thread remains available to
the same Codex account/home and workspace identity. Otherwise resume fails
visibly and `/clear` starts a new thread.

There is no native-session deletion, vendor-thread deletion, garbage
collection, portable-workspace rewrite, durable session grant, invocation
auto-replay, or database migration tool yet. Explicit conservative operation
recovery is described separately. Unknown future record versions and artifact
hash mismatches fail visibly.
