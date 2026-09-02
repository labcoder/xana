# M4 terminal management and semantic-parity evidence

> Status: Implementation complete; owner terminal verification pending
> Scope: M4-12

## Implemented boundary

- CLI, plain mode, and TUI project one shared typed command catalog. Surface
  capabilities explain unavailable interactions instead of silently omitting
  them, and `conversation` is canonical while `session` remains compatible.
- Terminal users can create, find, resume, continue, branch, move, archive,
  preview, attach to, and inspect eligible Conversations through shared Rust
  lifecycle commands with owner-specific managed/native receipts.
- Composer history, transcript search, and `@file` completion are bounded,
  workspace-scoped, cancellation-aware, and backed by the same discovery
  primitive. Completion grants no additional file authority.
- Rich content, immutable resources, artifacts, usage facts, execution facts,
  prompt-ledger facts, completion receipts, and accessibility fallbacks use
  shared semantic projections. Private `stream-json` preserves correlation and
  stdout/stderr separation without becoming a public protocol commitment.
- Connection creation now uses the canonical setup transaction: establish and
  validate the endpoint/account before model choice, review the draft, then
  commit configuration and credentials atomically or roll both back. Test,
  catalog refresh/repair, credential replacement/deletion, managed login,
  exact login cancellation, logout, selection, and removal expose bounded
  progress, typed failures, and redacted receipts.
- Client-owned localization uses shared semantic message families, bounded
  dynamic values, safe unknown-code fallback, pseudolocalization, and a narrow
  representative Spanish setup/approval/receipt path. Runtime policy remains
  locale-independent.

## Automated evidence

On Windows, after the implementation and documentation changes:

- `cargo test --workspace --all-targets --all-features --quiet` passed: 1,071
  library tests, 25 CLI tests, 4 settings CLI tests, and 20 Desktop tests; 6
  explicit manual/timing fixtures remained ignored.
- `cargo test --workspace --all-targets --no-default-features --quiet` passed
  with the same feature-independent totals.
- Focused connection fixtures cover empty/duplicate identifiers, connection
  establishment before model choice, dry-run, rollback, inaccessible secrets,
  stale catalogs, unavailable models, catalog repair, destructive
  confirmation, and independent config/credential/account/model health.
- The pinned Codex app-server fixture proves exact `account/login/cancel`
  correlation and keeps cancelled, completed, failed, and unknown attempts
  distinct.
- Command-catalog, plain/TUI projection, JSON/JSONL, history/search/completion,
  rich-content, usage, resource, and managed/native Conversation fixtures cover
  the remaining cross-surface contract.

One highly parallel full-suite run observed a transient rejection in the
same-home Desktop forwarding fixture. The focused fixture then passed
repeatedly and both complete feature matrices passed. This is retained as
honest evidence rather than being hidden; M4-24 should repeat the complete gate
on all supported CI targets.

## Owner verification still required

Exercise the documented full setup/manage/recover sequence in both plain and
TUI modes, including one native connection and one managed Codex connection.
Review no-color, ASCII, screen-reader/plain, cancellation, stale-catalog,
credential replacement/deletion, and unavailable-provider behavior. This
environment- and taste-dependent pass belongs to M4-24 and is not claimed by
deterministic tests.

## Principal commits

- `b27f42f` — typed connection-management control plane
- `539146f` — shared terminal-management command projection
- `c0bcbcd` — bounded history, transcript search, and file completion
- `0d838f8` — execution facts and completion receipts
- `4bbf7fa` — unified Conversation lifecycle commands
- `4908b19` — intentional blank initialization
- `6b9b6c2` — private streaming JSON projection
- `c4ca696` — canonical validated connection lifecycle
- `e7d3744` — exact managed-login cancellation
- `71bd463` — shared semantic localization copy
