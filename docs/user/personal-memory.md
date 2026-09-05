# Personal memory controls

Audience: Users. Authority: Descriptive.

Xana can retain facts you explicitly ask it to remember in a
[protected home](protected-storage.md). Records are separate from task history,
instructions, tools and permission grants. These controls run locally without
a model call. An unlocked protected home needs no additional authentication;
a locked or legacy home reports the prerequisite instead of creating plaintext
memory. Ordinary source files and settings remain ordinary files.

## Remember and inspect

In plain chat, TUI or Desktop chat, these direct requests are local controls:

```text
remember that I prefer short examples
remember for all conversations: I prefer Rust examples
what do you remember?
correct memory UUID: I prefer concise explanations with one example
move memory UUID to user
disable memory UUID
disable memory for this conversation
enable memory for this conversation
```

`remember that ...` stays in the current Conversation. Xana asks whether it
should apply everywhere without blocking or broadening it. `remember for this
conversation: ...` is also accepted. `remember in SCOPE that FACT` names an
explicit scope. This is a small deterministic grammar, not general natural-
language extraction: other wording remains ordinary model input. Quoted/tool/
child output is never processed as an owner memory command. Attached-image turns
remain model turns, not local memory commands.

Inspection returns at most eight short previews with IDs and an explicit
more/truncation indicator. For complete records, use Desktop's **Memory** panel,
`xana memory show UUID`, or paged `xana memory list`. User-wide, Profile-private,
Project and Conversation scopes are independent. Chat inspection combines only
the current Conversation's applicable scopes; owner management commands can
explicitly inspect another scope. This is local-owner control, not multi-user
authentication.

## Terminal and Desktop controls

```text
xana memory remember --scope user --text "I prefer concise answers"
xana memory list --scope user
xana memory list --after LAST_SEQUENCE
xana memory show UUID
xana memory correct UUID --revision 1 --text "I prefer concise answers with examples"
xana memory scope UUID --revision 2 --to project:PROJECT_UUID --confirm
xana memory disable UUID --revision 3
xana memory controls --scope conversation:CONVERSATION_UUID --no-memory on
xana memory controls --scope user --use on --learn off
xana memory export --scope user --output NEW_FILE.json
```

From this repository, prefix commands with `cargo run --locked --`. Plain/TUI
also support `/memory ...` between turns, through the same command parser.
For scripting a direct phrase without a chat session, use
`xana memory say --conversation CONVERSATION_UUID "remember that ..."`;
optional `--profile` and `--project` supply explicit inspection context.

Desktop → Panels → **Memory** (or the command palette) provides named scope
choices, exact scope entry, Refresh/Next page, record inspection, editable
statement/expiry, correction, disable, explicit scope move, three independent
controls and a readable export picker. Choosing a scope does not discard the
inspected record: Refresh browses it, whereas Move requires the separate scope
confirmation. Save controls requires a fresh inspection of that scope. Text
uses the component library's selection/copy and multiline editing controls.

Each record has a stable ID, checked revision, stated/inferred classification,
eligibility state, owner-request provenance, origin Conversation when available,
creation/change time and optional expiry. Explicit records are **stated** and
**active**. Changes archive the old revision as superseded. Concurrent stale
edits fail with a refresh instruction instead of overwriting newer work.

`--expires-at` is UTC Unix seconds. Expiry excludes a fact from eligible use
without erasing it. CLI/natural corrections preserve expiry unless explicitly
changed; CLI `--clear-expiry` removes it. Desktop loads the existing expiry into
the field; clearing that field explicitly removes it. Disable makes a record
stale/ineligible, not securely erased.

Use and learning permission are independent. **No-memory** overrides both
without erasing either saved setting. Any disabled applicable scope limits the
combined eligible view. A restored-home review gate also blocks eligible use
and learning; scope controls cannot clear that independent gate. Explicit owner
inspection/edit/export remains available for review. No-memory does not erase
Conversation history or change provider retention.

## Limits and what is not implemented yet

Statements are limited to 4,096 UTF-8 bytes, encoded records to 8 KiB, and pages
to 64 records. Eligible inspection examines at most 1,024 records in creation
order across the applicable scopes and reports when that bounded view is
incomplete. Explicit pages remain available beyond it. This is not a relevance
ranking system. Readable JSON exports contain current records (including inactive
ones), not revision history or a restore image, and are capped at 32 MiB.
Exports create a new private file, never overwrite one, and clean up their own
failed partial writes. The successful copy is outside managed encryption; store
or delete it deliberately. No Markdown mirror is created.

Corrections and scope/control changes apply on the next eligible read; existing
snapshots and in-flight work are not restarted. **Ordinary model prompts do not
yet automatically select these records.** Automatic learning, prompt injection,
cross-Conversation task recall, robust forgetting and undo remain separate work.
The learning checkbox is a persisted permission gate, not a running extractor.

Native local-control acknowledgements are saved in the Conversation. Managed
Codex controls acknowledge locally and store the memory record; they do not
send a vendor turn or manufacture vendor transcript history. After reopening,
inspect Memory for the durable result. If a receipt fails after a change,
inspect its record before retrying; there is no automatic effect replay.
