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
frontend snapshots already exist. Automatic memory, semantic processing and
cross-Conversation recall remain future work. Optional embeddings, graph stores,
RLM kernels, external memory vendors and automatic policy/Skill refinement are
not required. User and Architecture docs describe only implemented slices.

