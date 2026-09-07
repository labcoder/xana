# Scoped personal memory and bounded context

> Audience: Contributors and coding agents
> Authority: Prescriptive
> Status: Accepted

## Contract

Xana learns about its user without confusing personal knowledge with task
history, authored instructions, or a new identity. The engine stays headless.
Application policy owns selection, storage, processing routes and permissions;
clients expose the same typed commands and never become another memory writer.
This accepts the bounded design below, not all of Proposed 0008 or 0009.

### Context compilation

Compile each native request from stable identity/instructions, current execution
facts, protected recent history, eligible memory, tool/Skill declarations and
source-attributed evidence. Use the existing model-aware input/output/reasoning/
tool reserves and immutable ContextRecord/artifact seams. Oversized tool results
become bounded references before summarization. Never silently discard required
instructions, the current request or a pending tool exchange to fit a limit;
compact eligible history or report insufficient capacity before dispatch.

Report section costs, exclusions, provenance and unknown tokenizer/cache facts
without logging prompt content. Conservative estimates are not provider usage.
Cache affinity is an optimization, never permission to reuse stale scope,
corrections, capabilities or provider/model facts. No relevance-model call is
required on every message. Lazy capability selection, if introduced, must retain
deterministic manual discovery and an explicit override; eager bounded schemas
remain valid.

Semantic compaction must improve the existing deterministic checkpoint baseline
on fixed multilingual, correction and long-horizon fixtures while preserving
constraints, sources/ranges/hashes, completed work and unresolved work. Raw
history remains immutable. Missing authorized helper, failure or exceeded budget
retains the baseline or suspends explicitly, never fabricates a summary.
Managed runtimes own their history and compaction; Xana supplies only a bounded,
labeled selection at supported turn boundaries, not a second agent loop.

### Personal memory

Disclose automatic learning at setup and upgrade, with an obvious opt-out.
Learning and use are independent controls. A no-memory Conversation disables
both; it does not promise absent history or provider retention.

Ordinary user statements and cautious repeated behavior are eligible.
Distinguish stated from inferred facts, with immutable source provenance and
versioned candidate/active/superseded/stale/forgotten eligibility. Task procedures,
tool/file/browser claims, recalled text and child output are not automatically
personal truth. Sensitive traits or incidental sensitive/third-party information
require explicit permission. Learning never rewrites identity, policy or Skills.

Keep user-wide, Profile-private, Project and Conversation scope distinct.
Explicit wider intent may broaden scope; ambiguity remains narrow and asks
non-blockingly. Current instructions and permission ceilings always win.
Stable preferences remain until corrected or forgotten; temporal facts expire
for use independently of storage retention.

Provide inspect, correct, scope, disable, forget, safe undo and export through
Xana, including natural-language changes through the same governed commands.
No SQL or Markdown editor is required. Corrections apply next eligible turn;
already-dispatched work is not silently restarted.

Free-form owner language is interpreted by the conversational model through
`memory_lookup` and `memory_update`, not an English phrase recognizer or a hidden
pre-turn classifier. Native and supported managed adapters share a scoped service
with immutable host-bound owner input; tool arguments cannot invent identity,
provenance or permission. Keep deterministic CLI/UI controls for offline recovery.
Default-Ask may admit ordinary explicit current-Conversation saves without file
approvals; explicit deny remains authoritative. Broader, sensitive, uncertain,
corrective and destructive changes require exact review. Source attribution is
not a guarantee that a model understood consent; qualify semantic failures too.

Lookup is bounded and respects current scope/use/privacy controls. Mutation
rechecks source eligibility, controls and revision inside the committing
transaction, with durable idempotent receipts. Refresh native memory context
before the next model request after a change; never append superseded records
to an old prompt layer. Memory already handed off to a vendor cannot be erased by
changing a local record. An unavailable tool must not fall back to workspace files.
Do not advertise personal-memory tools when no protected owner is attached.
Readiness guidance must match the actual capabilities, and permanent
unavailability must not be treated as an ordinary retryable argument error.
Unknown-fact questions are not permission to write a guessed fact. Ordinary
fresh setup remains distinct from explicitly initializing protected storage.

The background learner interprets language into closed ordinary preference values
(response detail, examples, units, theme and supported development languages).
Only canonical values with stated whole-source attribution may auto-activate;
arbitrary prose, uncertain/inferred claims and sensitive suggestions do not gain
that authority. Foreground saves retire background interpretation of the same
owner input. Do not add a helper call merely to understand an explicit save.

Forgetting invalidates recall, derived selections, pending extraction and stale
jobs. Version/suppression checks prevent automatic relearning from old sources,
compaction, task recall or backups. Raw-history deletion is a separate operation.
A new explicit user request may restore learning; automatic replay may not.
Restore reconciles exclusions before enabling recall or work.

Extraction uses small incremental batches under an authorized route and shared
usage limits. Commit rechecks scope, sources, no-memory controls, forgetting,
permission, budget and cancellation. No route means visible pending processing,
not a secret provider substitution or unusable chat. No vendor memory scraping
or undocumented subscription API. Subscription-only helper support requires
separate demonstrated supported controls.

### Recall and source knowledge

Start with exact identifiers, metadata and local lexical retrieval. Return
bounded cited source/range evidence, rechecking freshness and authority when
materializing it. Same-Project eligible task recall may cross Conversations;
unrelated Projects, Profile-private and ungrouped work never broaden implicitly.

Index only selected Markdown/text roots and eligible Xana-managed evidence.
Canonical path identity, links, changed/deleted files, size limits and stale
indexes are explicit. Selecting a root permits indexing, not arbitrary provider
disclosure or editing originals. No ambient home scan or Obsidian dependency.
Originals remain ordinary files; managed indexes follow
[protected storage](0024-encrypted-managed-content-and-recovery.md).

## Initial bounds and proof

Personal injection is at most 2,048 estimated tokens and 5% of usable input,
whichever is smaller. Recall starts at 8 hits / 64 KiB; source ingestion at
2 MiB/text file and 100 MiB/batch, with explicit bounded continuation.
These are defaults, not permission to exceed a narrower request budget.

Use fixed >=40-case semantic and retrieval suites. Require >=95% retained
required constraints, all correction/scope canaries, >=90% useful cited retrieval
on answerable cases, >=95% abstention on unanswerable cases and resolvable
citations. Report uncertainty and costs; fixtures are not universal accuracy.

Long-history projections must page at the reader and client boundaries, bound
aggregate bytes/events/caches, preserve identity/anchors/drafts and avoid
complete-history cloning per delta. Measure 10k/100k variable Unicode histories
with artifact outliers in release builds. Virtual painting alone is insufficient.

## Scope and implementation

Context budgets, deterministic compaction, artifact references and bounded
frontend snapshots already exist. The initial delivered subset additionally
preserves complete required instructions or rejects assembly, publishes per-request
root/child section costs, and retains oversized tool output behind typed artifact
references in live and restored clients. Retained client windows and saved-history
paging are bounded; the legacy store limit has not been raised. See
[Architecture](../architecture/README.md) and [project context](../user/project-context.md)
for the current behavior. This proposal remains Accepted, not Implemented:
explicit owner memory controls now persist scoped stated facts, immutable
revision history, temporal eligibility, independent use/learning/no-memory
flags and safe readable exports. CLI/plain/TUI/Desktop share the same governed
backend. Free-form requests now use turn-bound semantic memory tools; explicit
CLI/UI management remains deterministic. See [personal memory](../user/personal-memory.md).
Corrections apply before the next native model request and next managed turn.
Bounded automatic learning now uses
explicit native helper approval, typed ordinary-preference activation,
inactive ambiguous/inferred candidates and source/consent fences; sensitive
suggestions are not copied without explicit retention permission. Forgetting
persists suppression, separates reviewed native source deletion, and reconciles
known later exclusions on restore. Supported managed text handoff includes
current scoped records without an extra turn and makes no vendor erasure claim.
The narrow governed-candidate workflow now shares protected Memory/inert-Skill
envelopes, exact review/diff/reject/archive/undo and stale source/base/privacy
checks across owner clients. Typed automatic preferences carry candidate proof;
sensitive helper payloads are not copied. Explicit owner Skill drafts remain
database-only and review never installs, loads or executes them. General harness
promotion and remaining integrated quality/native platform evidence are not
inferred complete by these slices. Optional embeddings, graph stores,
RLM kernels, external memory vendors and automatic policy/Skill refinement are
not required. User and Architecture docs describe only implemented slices.

Native semantic compaction now has a bounded no-tool helper, exact-route
evaluation/explicit opt-in, durable usage, source/privacy commit checks and safe
fallback. No route is promoted by mocked tests; its separate real-model quality
measurement and owner assessment remain required before enabling it. See
[evaluated semantic compaction](../user/semantic-compaction.md).

The P0 follow-up adds explicit adapter-owned helper generation controls,
independent answer/reasoning bounds, model-aware preparation, and a versioned
repeated-compaction evaluation. Original-proof paging and indexed/lazy memory
selection reduce local work without a persistent trusted cache. Qualification
is still a real-route gate, not a consequence of these implementation changes;
native-platform, loaded-client and semantic owner assessment remain distinct.

Project task recall and selected Markdown/text knowledge now use the existing
protected database's bounded lexical index, exact original references/hashes,
current Project/frozen-Profile scope, explicit broader inclusion and separate
notes-disclosure grants. History and registered text artifacts refresh
incrementally; selected files remain ordinary editable originals. Rebuild and
source deletion discard derived index state without deleting unrelated notes.
Existing native branches inherit ancestor quarantine for automatic reuse.
See [implemented recall architecture](../architecture/recall.md) and
[owner controls](../user/project-recall.md); broad embedding/media ingestion
and general managed-agent tool bridging remain outside this implementation.
The supported Codex bridge is deliberately restricted to the two personal-memory
tools and does not delegate arbitrary Xana tools to the managed inner loop.
