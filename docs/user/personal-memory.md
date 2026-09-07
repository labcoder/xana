# Personal memory controls

Audience: Users. Authority: Descriptive.

Xana can retain facts you explicitly ask it to remember, and process eligible
user statements through an explicitly authorized learning helper, in a
[protected home](protected-storage.md). Records are separate from task history,
instructions, tools and permission grants. These controls run locally without
a model call. An unlocked protected home needs no additional authentication;
a locked or legacy home reports the prerequisite instead of creating plaintext
memory. Ordinary source files and settings remain ordinary files.

## Remember and inspect

In plain chat, TUI or Desktop chat, these direct requests are local controls:

```text
remember that I prefer short examples
my favorite color is red. remember that.
my favorite color is red, remember that, ok?
could you please remember that I prefer short examples?
please remember my preference for short examples
remember for all conversations: I prefer Rust examples
what do you remember?
correct memory UUID: I prefer concise explanations with one example
move memory UUID to user
disable memory UUID
forget memory UUID
restore memory UUID
disable memory for this conversation
enable memory for this conversation
```

`remember that ...` stays in the current Conversation. Xana asks whether it
should apply everywhere without blocking or broadening it. `remember for this
conversation: ...` is also accepted. `remember in SCOPE that FACT` names an
explicit scope. This is a small deterministic grammar, not general natural-
language extraction: it separates explicit request wrappers (`please`, `can you`,
`could you`, `would you`, `will you`) from a stated fact, and accepts personal
statements followed by a comma, period, semicolon or em dash and `remember
that/this/it`. Polite tags such as `, please` and `, ok?` are not part of the
saved fact. Bare recall questions such as `Can you remember my favorite color?`
do not save a new record. Recognized conditional or contradictory save requests
and unresolved scope qualifiers ask for clarification locally without saving or
dispatching file tools. These checks are conservative templates, not a general
language or consent classifier; other wording remains ordinary model input.
A suffix request
defaults to this Conversation; `My favorite color is red. Remember that for all
conversations.` explicitly selects user scope. Quoted/tool/
child output is never processed as an owner memory command. Attached-image turns
remain model turns, not local memory commands.
This includes image turns prepared by a named vision specialist: its generated
description is available to the conversational model but cannot become your
memory command or learning source. Only the original text you wrote is eligible
for personal-memory selection and learning.

Restarting and resuming the same Conversation retains its scoped memories. A
new Conversation receives user-wide memory but not another Conversation's private
facts. Recall uses eligible memory data directly, without `read_file` approval.
Xana does not use `AGENTS.md`, instruction files, or an ad-hoc workspace
`user_prefs` file as its memory store. Listing or editing workspace files is
not part of saving a personal fact; you should not need to approve file tools
for a recognized memory control. Ordinary filesystem work still follows your
[permission policy](permissions.md).
Saving returns a short receipt with the statement, scope and memory ID, not a
model's promise. Use that ID to correct, move or forget the saved fact.

On a legacy home, explicit remember requests fail locally with setup guidance;
they do not ask a model to save a plaintext substitute. `xana doctor` warns that
personal memory is unavailable. Native prompts and managed Codex turns include
the current readiness fact and distinguish disabled use from unavailable storage.
Run `xana storage status` and `xana storage migrate` for a read-only preview;
application requires a recovery key and your exact reviewed digest. From a source
checkout, use `cargo run -- storage status` or `cargo run -- storage migrate`.
No migration happens merely by asking Xana to remember something.

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
xana memory forget UUID --revision 4
xana memory restore UUID --revision 5 --confirm
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

## Automatic learning

Learning is the disclosed default permission, independently of using memory;
it does not select or authorize a paid provider. Setup and existing-installation
startup surfaces disclose this policy; TUI shows a local status message and
Desktop shows a dismissible notice with a Memory-controls shortcut. These
notices are not model input or saved user turns, and dismissing them does not
change permission. Authorize the exact native
connection/model once before source text can be sent for extraction:

```text
xana memory learning-status
xana memory learning-route --connection ollama --model YOUR_MODEL --confirm
xana memory process
xana memory learning-route --connection ollama --model YOUR_MODEL --confirm --disable
```

Only accepted user-input edges enqueue statements; assistant, tool, browser,
file, recalled and scheduled output do not. The encrypted incremental queue
holds at most 1,000 sources, each at most 8 KiB. A helper sees at most eight
sources in one text-only request, with no tools. Missing/unavailable routes,
budget exhaustion and interruptions leave visible pending work; they do not
disable offline memory controls. Ordinary native Conversations process batches
while idle, coalescing small batches for up to 30 seconds; managed Conversations
can process them after a turn's idle delay, and
`memory process` explicitly processes a smaller pending batch. No helper is
silently selected from a Codex subscription.

An ordinary exact whole statement activates only under the documented initial
allowlist: “I prefer concise responses”, “I prefer detailed responses”, “I
prefer examples”, “I prefer metric units”, “I prefer dark mode”, “I prefer
light mode”, or “I use Rust/Python/TypeScript” (case and final `.`/`!` may vary).
These records stay Conversation-scoped. Other suggestions not flagged sensitive,
including inferences, are inactive candidates, never instructions or permission.
For suggestions the helper flags as sensitive, Xana retains only a redacted
candidate envelope, not the quoted fact; an explicit owner remember request is
required for intentional sensitive retention. Conflicting duplicate suggestions
use the sensitive classification rather than copying a less restrictive version,
and any conflicting inferred classification keeps the quote staged regardless
of suggestion order. Classification
can be wrong: an unflagged sensitive quotation can remain in an inactive,
encrypted candidate. It cannot pass the ordinary-statement allowlist or become
eligible context automatically, but inactive does not mean absent from storage.
Inspect and forget unwanted candidates, or disable learning when this residual
risk is unacceptable. This policy does not infer a global preference from one
task instruction, and no-memory does not erase the original Conversation.

Inspection shows stated/inferred classification and source identity. Scope,
control, source, route and forgetting checks run again before the transaction
commits. Repeated source IDs cannot create duplicate processing. Learning shares
the background lane and daily token allowance with other maintenance: at most
8,192 reserved input/output tokens per job, 32,768 background tokens per day,
120 seconds and one active helper. Lower owner budgets still apply. Foreground
input preempts maintenance; an interrupted provider request is not treated as
free just because its final usage is unknown.

The helper's current connection configuration is checked before dispatch and
again before committing suggestions; replacing an endpoint or credential
reference cannot inherit an old route approval. Source/control/restore changes
conservatively retire older queued statements instead of silently rebasing
them under new authority. `memory learning-status` shows `excluded_after_change`
and a metadata-only `last_retirement` reason; retirement is bounded to 64 sources
per pass, and sources remaining beyond that page still cannot be dispatched
when ineligible.

## Review learning candidates and inert drafts

Candidate review is an offline owner control in CLI/plain/TUI and Desktop's
Memory panel. It never calls a model or gives a draft tools or permissions:

```text
xana memory candidate list --scope conversation:CONVERSATION_UUID
xana memory candidate list --after LAST_SEQUENCE
xana memory candidate show CANDIDATE_UUID
xana memory candidate diff CANDIDATE_UUID
xana memory candidate approve CANDIDATE_UUID --revision 1
xana memory candidate reject CANDIDATE_UUID --revision 1 --reason "Not accurate"
xana memory candidate undo CANDIDATE_UUID --revision 2
xana memory candidate archive CANDIDATE_UUID --revision 3
xana memory candidate stage-skill --scope user --name example --markdown "# An inert procedure draft"
```

Use `/memory candidate ...` between turns in plain/TUI, or the **Learning
candidates** area of Desktop Memory. Select the exact scope, refresh its bounded
page, inspect the proposal/provenance/diff, and confirm the inspected content
before acceptance. Candidate IDs are distinct from their target memory IDs.

A candidate records its original source identity and hash, target and base
revision, scope, privacy/consent generation, classification, validation rule,
content hash and ordered owner/policy review events. List returns at most 32
metadata-only summaries; Show/Diff explicitly materialize one proposal.
Inferred or ambiguous memory remains inactive until approved. Approval preserves
its stated/inferred classification and exact scope; it cannot choose a broader
scope, change permissions, select a provider or install a tool. Explicit memory
correction/scope commands remain separate owner actions.

Automatically activated allowlisted facts also have candidate records with the
deterministic rule and source proof. **Undo** revokes the unchanged published
fact; it never restores old text over a subsequent correction. **Reject** retires
a pending proposal without changing a memory target. **Archive** retains review
history rather than securely erasing it; disable/undo an unchanged active fact
before archiving its candidate. A corrected, disabled or forgotten publication
can be archived without touching the newer target.

Scope/control changes, source replacement/exclusion, correction, forgetting and
restore invalidate incompatible proposals. The privacy-generation fence is
conservative: accepting a memory can also invalidate other older staged
proposals. Refresh reports the conflict but does not rebase old evidence; use a
fresh explicit remember/correction or stage a fresh draft with current consent.
Undo can likewise refuse after such a change; the exact memory's direct Disable
or Forget control remains available. Restored-memory review does not reauthorize
old candidate tokens. Pre-existing inactive candidates imported from older
protected schemas are inspectable but cannot fabricate missing extraction proof
or become newly approved through the candidate path.

Sensitive metadata-only candidates have no recoverable quotation. Even
`--confirm-sensitive` cannot reconstruct it: intentionally remember the fact in
a fresh owner request. Forgotten/excluded candidate sources also hide payloads,
pre-images and rejection text in inspection, and prevent the old memory
inspection/export and prompt-selection surfaces from exposing those candidate
payloads. Explicitly owner-created memory records retain their separate
owner-inspection contract. A later exact owner Restore or correction makes the
current active fact independent of its old candidate publication; the current
fact is visible and usable, but the excluded candidate payload, pre-image and
old approval/undo proof stay unavailable.

Skill drafts contain at most 32 KiB of inert Markdown in the protected database,
not a file under `.agents` or a Skill/plugin directory. Acceptance means
**reviewed only**: no discovery, prompt assembly, loading or execution follows.
Inspect/copy the reviewed text deliberately and use the existing explicit
Skill/plugin authoring and installation lifecycle if it should become a real
Skill. Draft creation is currently an explicit owner action, not automatic
procedure extraction. General prompt, route, policy, executable or harness
self-modification is not supported by candidate review. Envelopes are bounded
to 64 KiB and 64 revisions; inspection is not a background full-catalog load.

## Memory in a conversation

Before a native or managed turn, Xana selects current records from the applicable
scopes using bounded deterministic matching, without an extra relevance-model
call. The personal context allowance is the smaller of 2,048 estimated tokens
and 5% of the usable input budget, including provenance and wrappers. Native
context also respects the complete prompt budget. When the managed model's
usable window is unknown, Xana uses a conservative 16,384-token accounting
allowance; this is not a claim about Codex's actual window.

Native prompts label records as untrusted personal data after mandatory
instructions; the context ledger reports their cost. Codex receives the bounded
current records with the actual user message through its supported text input,
not through a second agent turn. IDs, revisions, scopes and provenance travel
with selected records, not the whole memory store. Model changes still use the
same vendor thread. Corrections apply on the next turn, without interrupting an
answer already underway. Previously transmitted vendor content remains
vendor-owned and cannot be claimed erased.

## Forgetting, source deletion and restored backups

Disable makes a fact inactive. **Forget** also persists a suppression record,
invalidates pending derived work and quarantines its originating Conversation
from automatic recall, extraction and compaction. Local history inspection
remains possible. If a Conversation already received a now-forgotten fact or
memory use is switched off after a handoff, Xana requires a new Conversation
before model dispatch; continuing old native/vendor context would silently
reuse the information. Exact explicit Restore re-enables the fact, not its old
source or stale jobs.

Raw native history deletion is a separate reviewed operation:

```text
xana memory delete-source CONVERSATION_UUID
xana memory delete-source CONVERSATION_UUID --review EXACT_PREVIEW_TOKEN
```

The preview is bound to the source revision, and deletion refuses an active
writer. Shared artifacts are retained, not garbage-collected. Desktop's Memory
record inspector exposes the same preview and confirmation. Neither deletion
nor forgetting promises secure physical erasure, vendor deletion or removal of
old backup copies.

Restoring an older backup onto the same compatible home reconciles later known
forgetting/source-deletion exclusions before making it available. Eligible
memory stays gated for review; foreign backups cannot contain decisions made
after they were captured. Inspect records, forget anything that should stay
excluded, then use `xana memory review-restore` and repeat it with the exact
`--review` token. This review does not re-enable restored automation or clear
the separate usage-accounting review.

## Bounds and remaining limits

Statements are limited to 4,096 UTF-8 bytes, encoded records to 8 KiB, and pages
to 64 records. Eligible inspection examines at most 1,024 records in creation
order across the applicable scopes and reports when that bounded view is
incomplete. Explicit pages remain available beyond it; prompt matching is over
this bounded eligible set, not an unbounded semantic search. Readable JSON exports contain current records (including inactive
ones), not revision history or a restore image, and are capped at 32 MiB.
Exports create a new private file, never overwrite one, and clean up their own
failed partial writes. The successful copy is outside managed encryption; store
or delete it deliberately. No Markdown mirror is created.

Corrections and scope/control changes apply on the next eligible read; existing
snapshots and in-flight work are not restarted. Governed candidate review and
inert Skill drafts do not rewrite identity, installed Skills or permissions. Task recall remains a
separate source-evidence system, not personal truth.

Native local-control acknowledgements are saved in the Conversation. Managed
Codex controls acknowledge locally and store the memory record; they do not
send a vendor turn or manufacture vendor transcript history. After reopening,
inspect Memory for the durable result. If a receipt fails after a change,
inspect its record before retrying; there is no automatic effect replay.
