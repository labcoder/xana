# Project evidence and selected notes

> Audience: Xana users

Recall finds earlier task evidence without turning it into personal memory.
It requires an unlocked protected home and a native Conversation with its
frozen Profile. The encrypted lexical index is derived data; every returned
excerpt is re-read from its original source and checked against its citation.

## Earlier Project work

Use an exact native Conversation ID from `xana conversation list`:

```text
xana recall --conversation ID refresh
xana recall --conversation ID search "deployment decision"
```

Refresh processes one bounded batch per eligible Conversation and prints
`inspected`, `indexed`, `skipped`, and `pending`. Repeat while `pending` is
true. New entries are indexed on later refreshes; searches do not silently
crawl history or call a provider. No match means no fresh eligible indexed
evidence, not proof that the information never existed.

`xana recall --conversation ID rebuild` atomically discards only the current
scope's derived index and refresh cursors. Original history, selected roots,
notes, and disclosure grants remain unchanged. Run history and selected-root
refresh again to rebuild the searchable evidence.

The default is the same Project and the same frozen Profile identity. An
Ungrouped Conversation can retrieve only its own evidence. Include a particular
same-Project Conversation with a different Profile only when intended:

```text
xana recall --conversation ID include SOURCE_ID
xana recall --conversation ID include SOURCE_ID --remove
```

Unrelated Projects cannot be included. Reassigning a Conversation, revoking an
inclusion, forgetting its source, or deleting history prevents stale evidence
from being returned. A source-privacy barrier can suspend recall for the
consumer Conversation itself; original history remains separately inspectable.
Existing native branches inherit quarantine from a forgotten or deleted ancestor,
so copied history cannot silently reintroduce the information. Missing declared
ancestors, cyclic lineage, and chains beyond 32 source identities fail closed;
this is automatic-reuse quarantine, not deletion of branch history.

Results contain original Conversation/entry/artifact IDs or selected-root/relative-file
IDs, a source hash, exact UTF-8 byte ranges, and bounded excerpt text. The native
`recall` tool uses the same checks and ordinary tool permission policy. Retrieved
text is untrusted evidence: it cannot grant permissions, define instructions,
or prove that work was completed. Managed Codex has no Xana-native tool bridge;
use explicit local recall inspection when preparing an intentional handoff.

Registered UTF-8 `text/plain`, `text/markdown`, and `application/json` artifacts
up to 2 MiB are eligible under their registering Conversation. Their immutable
content hash is checked by the existing artifact reader. Arbitrary unregistered
blobs and other media are not scanned or converted by recall.

## Ordinary editable notes

Select an existing Markdown/text directory; selection authorizes local indexing,
not sending notes to a model:

```text
xana recall --conversation ID notes select PATH
xana recall --conversation ID notes list
xana recall --conversation ID notes refresh ROOT_ID
```

Repeat `notes refresh ROOT_ID` while `pending` is true. Each invocation processes
at most 1,000 files and 100 MiB, then saves an encrypted metadata-only cursor;
restarting Xana resumes that cursor without rereading completed batches. Ctrl+C
retains committed progress. A crash may replay the last bounded batch, safely
reusing source hashes. Incomplete scans never remove unprocessed indexed sources.
Missing-source cleanup occurs only when the captured inventory finishes.

The inventory fixes filenames for one scan, but each file's current content and
identity are checked when read. Newly added filenames enter the next complete
scan, or use `notes refresh ROOT_ID --restart` to take a new inventory now. Changes
to privacy or selection policy invalidate the old cursor and require that explicit
restart; a revoked root cannot be resumed. Original files are never modified.

`notes import` is an alias for `notes select`; files stay in place. Only `.md`
and `.txt` UTF-8 regular files are indexed. No home-directory crawl, Obsidian
dependency, synchronization service, or plaintext personal-memory mirror exists.
Original files remain ordinary files for your editor. Xana's index is encrypted;
the originals you selected are not encrypted or modified by Xana.

The optional `notes create` command explicitly creates the ordinary
`data/notes` directory under your configured Xana paths. It does not implicitly
select or disclose that directory. Use the returned path with `notes select`.

To allow one selected root's results to reach one native provider/model:

```text
xana recall --conversation ID notes disclose ROOT_ID --connection NAME --model MODEL --confirm
xana recall --conversation ID notes disclose ROOT_ID --connection NAME --model MODEL --confirm --remove
```

This grant is bound to the exact configured recipient and model, not merely a
display name. Normal conversation/provider authorization still applies. A local
CLI search displays eligible notes without sending them anywhere. Revoking a
root removes its index and authorization but never deletes the originals:

```text
xana recall --conversation ID notes revoke ROOT_ID
```

Explicit `notes export ROOT_ID NEW_DIRECTORY` writes fresh indexed originals
into a new ordinary directory. It refuses existing destinations and stale
sources. Cancellation or a filesystem error leaves any partial new export for
inspection; it never overwrites an existing tree. Exported files are plaintext.

## Bounds and freshness

Search accepts 1–8 literal terms in at most 512 bytes, returns at most eight
4-KiB excerpts and 32 KiB total, and checks at most 64 candidates. This is local
lexical retrieval, not embedding/vector similarity or a model-generated answer.

A history refresh has at most 256 Conversation candidates, 128 records and
2 MiB per Conversation batch. A selected-root processing batch allows 1,000 files,
2 MiB per file and 100 MiB total, with explicit continuation. Its metadata inventory
allows 10,000 directory entries, depth 32, 2 MiB serialized metadata and relative
paths up to 2,000 UTF-8 bytes; select narrower roots beyond those bounds. Ctrl+C
cancels local indexing; already committed progress is retained. Files that are
invalid UTF-8, too large, linked, missing, or changed are skipped or rejected.
Root/citation checks run again when materializing results, so an editor change
or deletion causes abstention until a fresh index is available.

Symbolic links and Windows reparse points are not followed. File reads check
canonical containment and opened-file identity before and after reading.
These checks are not an operating-system sandbox against a hostile account
that can concurrently replace arbitrary ancestor directories.

See [personal memory](personal-memory.md), [permissions](permissions.md), and
[protected storage](protected-storage.md) for the separate contracts.
