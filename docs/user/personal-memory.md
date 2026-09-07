# Personal memory controls

Audience: Users. Authority: Descriptive.

Xana can retain facts you explicitly ask it to remember, and process eligible
user statements through an explicitly authorized learning helper, in a
[protected home](protected-storage.md). Records are separate from task history,
instructions and permission grants. Explicit CLI/UI controls run locally without
a model call. An unlocked protected home needs no additional authentication;
a locked or legacy home reports the prerequisite instead of creating plaintext
memory. Ordinary source files and settings remain ordinary files.

## Remember and inspect

In chat, ask naturally, in your language. For example:

```text
My favorite color is red; don't forget that.
Recuerda que prefiero respuestas breves.
For all conversations, remember that I prefer Rust examples.
What do you remember about my preferences?
My favorite color changed to blue. Please update that memory.
Please forget my favorite color.
```

The selected model uses `memory_lookup` and `memory_update` in its normal tool
loop. There is no separate classifier or extra helper call before every message.
Understanding still depends on the model: these are examples, not a promise that
every paraphrase will be understood. When context already answers a question,
Xana need not call a tool. If the fact or the meaning of “this” is unclear, it
should ask; a save must cite the original text in your current message.

Conversation is the narrow default. User means across all eligible conversations;
Project and Profile refer to the current named scope. Broader, sensitive,
uncertain, corrective and destructive changes need exact approval. Under the
default `ask` policy, ordinary explicit Conversation saves and eligible lookups
do not need file permissions. Explicit deny rules still win. Review identifies
the memory action, scope, statement and exact revision where applicable.

Restarting and resuming the same Conversation retains its scoped memories. A
new Conversation receives user-wide memory but not another Conversation's private
facts. “Learned” describes how a fact was acquired, not another global store;
automatic learning is separate from explicit saves.

Memory lives in Xana's protected store—not `AGENTS.md`, a workspace `user_prefs`
file, or Codex's own memory. Only a committed tool receipt proves persistence.
Repeated equivalent updates within one operation return the same receipt without
inserting another record. The model's promise alone does not prove a save.
Do not approve unrelated file writes merely to remember a preference.

Only the original owner input supplies provenance. Vision-specialist descriptions,
tool/browser results, child output and quoted instructions are not owner authority.
The model interprets intent and sensitivity; exact source checks alone cannot prove
it understood them correctly. Use explicit controls below for reliable recovery
when a model struggles.

Without unlocked protected storage, tools report unavailable and save nothing;
they do not create a plaintext substitute or migrate your home.
`xana doctor` and `xana storage status` report readiness.
`xana storage migrate` previews an existing-home migration; applying it requires
your recovery key and exact reviewed digest. From this checkout use
`cargo run -- storage status`. There is no extra authentication while the
protected home is already unlocked.

Lookup returns at most eight previews of 256 characters, scanning at most 64
current-scope records per call with an explicit continuation cursor. Use a short
literal query or an exact record ID. This is bounded lexical lookup, not an
embedding search; a page with no matches and a continuation is not proof that
nothing is stored. Full records remain available through Desktop's **Memory**
panel, `xana memory show UUID`, and paged `xana memory list`.
Use/no-memory and restore-review gates apply to model lookup; explicit owner
management remains available separately.

Managed Codex uses its supported experimental dynamic-tool bridge; Codex still
owns the inner loop. New Xana-created threads register the memory tools.
Older threads without this registration are retained but require a new
Conversation for the new contract; follow the displayed instruction instead of
deleting vendor history. Real-account compatibility is a separate manual check
from protocol mocks.

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

The authorized helper can interpret different languages into closed ordinary
preferences: concise/detailed responses, examples, metric/imperial units,
dark/light theme, and Rust/Python/TypeScript use. Only stated, nonsensitive,
whole-source suggestions with one of these values activate automatically, in
Conversation scope. Active text is the canonical preference value, not arbitrary
helper prose; the original source identity remains attributable. This replaces
the old English sentence allowlist.

Other suggestions remain inactive candidates. Sensitive classifications produce
redacted metadata, not a copied quotation; intentional sensitive retention needs
fresh explicit owner review. Conflicting duplicate claims or preference values
cannot activate by arriving first. Classification can be wrong: an unflagged
sensitive quotation may be retained as an encrypted inactive candidate, and a
model can misclassify a preference. Inactive does not mean absent from storage.
Inspect/forget unwanted candidates or disable learning if this residual risk is
unacceptable. A successful explicit save retires background processing of that
same input, including a helper result already in flight.

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

Automatically activated typed preferences also have candidate records with the
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
same vendor thread. Native context is reselected before each model request, replacing old personal
memory layers after a correction. Codex receives corrections in tool results and
fresh selection on its next turn; Xana cannot rewrite its already-sent context. Previously transmitted vendor content remains
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

Native semantic tool calls/results are saved in the Conversation. Managed
Codex retains its own tool transcript; Xana stores committed mutation receipts
with its original owner-request identity, not a fabricated vendor transcript.
Explicit CLI/UI management remains local. After reopening, inspect Memory for
the durable result. If a response is interrupted after a possible change, inspect
its record before starting a new request; Xana does not replay uncertain effects.
