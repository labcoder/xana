# Protected managed storage

> Audience: Contributors and coding agents  
> Authority: Descriptive

`storage::ProtectedStore` owns one SQLCipher connection and zeroizing secret
bundle behind an `Arc<Mutex<Option<Database>>>`. Clones share revocation, not a
process-global key cache. The composition edge selects OS custody or the explicit
recovery-file environment setting. `Agent` remains unaware of either.

`usage_budget` receives an owned store and dispatch attribution from composition;
`storage::usage` serializes admission and settlement in immediate transactions.
Per-request receipts, conservative unknown reservations and managed cumulative
observations remain distinct from the existing provider/account observation API.
Canonical opens upgrade the accounting schema under an exclusive owner lease;
inspection of a recovery snapshot does not upgrade its schema. See the
[usage policy guide](../user/usage-budgets.md).

`memory::MemoryOwner` is a capability injected only into trusted owner-input
adapters. CLI/plain/TUI and Desktop management share it; native owner turns and
managed owner turns intercept the bounded direct-control grammar outside the
Agent loop. It is never a model tool or child-Agent dependency. `storage::memory`
owns indexed records, immutable revision history and scoped controls. Current
records validate their indexed ID/revision/scope before exposure. Immediate
transactions serialize checked edits; read transactions hold coherent eligible
snapshots and exports. No SQL, provider, key-custody or filesystem work runs in
Desktop render methods.

Schema 3 adds memory tables to accounting schema 2. Canonical upgrades from 1/2
require the exclusive lifecycle lease, commit atomically, close while exclusive,
then reopen through lifecycle checks. Recovery inspection never upgrades.
Statements/encoded records/pages are bounded at 4,096 bytes/8 KiB/64 records.
Eligible inspection merges at most four covering-index scans of 1,025 integer
sequences, then reads at most 1,024 matching bodies, returning at most 64 active
unexpired records. It does not sort an unbounded set of BLOBs in SQLite. A
bounded incomplete view is explicit, not a relevance guarantee. Chat previews
further cap this to eight records and 256 characters per statement. Exports are
coherent create-only private JSON copies capped at 32 MiB, with identity-checked
cleanup of failed output; no plaintext mirror or auto-import exists.

Use/learning/no-memory flags are independent per User/Profile/Project/
Conversation scope. Any applicable restriction and the restore-review gate
limit eligible use. Explicit owner review remains possible. Correction changes
the next eligible read, never an in-flight snapshot. Automatic selection,
extraction and robust forgetting are not implemented by these controls; see
[personal memory](../user/personal-memory.md).

```mermaid
flowchart TD
    APP[Application / Desktop control plane] --> CUSTODY[OS custody or independent age recovery]
    CUSTODY --> STORE[ProtectedStore: revocable connection owner]
    SESSION[DurableSession / existing reducer] --> STORE
    RECORDS[Private records / managed handles / composer history] --> STORE
    OWNER[Owner memory controls: CLI / TUI / Desktop] --> MEMORY[MemoryOwner: scopes, checked revisions, eligibility]
    MEMORY --> STORE
    STORE --> DB[SQLCipher: records, ancestry, bounded catalogs]
    ART[ArtifactStore: existing authority and immutable IDs] --> STORE
    STORE --> AGE[age: opaque encrypted artifact objects]
    AGE --> RANGE[Authenticate whole stream; retain requested range]
    RANGE --> VIEW[Authorized preview / provider input / explicit export]
```

Homes without the explicit protected bootstrap retain the legacy JSONL/files
backend. A present but incomplete, locked, unsupported, unauthenticated or
unavailable protected store is an error, never a legacy selector. Database
identity and schema version are authenticated before use. Schema creation is one
transaction; activation follows an actual recovery-envelope verification.

SQLite immediate transactions serialize commits. Independent connection owners
hold shared lifetime leases; each native Conversation has one exclusive writer
and append checks the inspected revision. Lifecycle operations require the
exclusive home lease. Lock first drops requesting clones' usable key/connection;
if any independent owner remains it reports failure, not Locked. Frontends must
stop work and release content projections before this final operation.

Native records retain their original parser/reducer and size/record limits.
Encrypted ancestry metadata permits bounded message-page selection without
loading every body, but full execution restore still has the existing bounded
history limit. This is not the future unbounded-history/retained-worker design.

Artifacts use opaque UUID filenames, an encrypted content-hash manifest and
standard age streaming encryption. Whole-object authentication, length/hash and
file identity checks precede successful ranges/exports. Explicit export is
create-only and cleans only its own failed output. Managed image input uses
data URLs with an independent 32 MiB outbound RPC bound; inbound Codex frames
retain the 2 MiB bound. No transparent editor temporary-file decryption exists.

See [protected storage](../user/protected-storage.md) for privacy boundaries and
[native storage build inputs](../contributing/native-storage.md) for exact pins.

## Generation lifecycle

`storage::migration` inventories/hashes a selected legacy data directory, verifies
independent recovery before fencing, preserves ordinary settings/package code,
and imports records through existing history semantics. Unknown derived files
become explicit encrypted archives. Config version 5 blocks older readers;
a bounded sibling restart journal blocks current admissions during conversion
and recoverable rename gaps. Source and prepared generations remain distinct.
Windows writer locks are reacquired after directory renames and the source is
revalidated before activation. An unexpected mutation fails closed.

`storage::backup` uses keyed SQLite backup connections with matching SQLCipher
settings and separately copies immutable ciphertext. Full page authentication,
structural/referential validation and complete artifact authentication precede
publication and retention cleanup. A shared backup-directory lock serializes
maintenance. Count/age/byte bounds never delete the last usable copy before its
replacement verifies. These are explicit/due-checked maintenance calls, not an
independent background scheduler.

`storage::restore` publishes a verified protected generation while leaving
ordinary files untouched and retaining the prior encrypted generation. A
durable review-required marker blocks current memory eligibility and prevents future automation services from
treating restored eligibility/grants as current authority. There is no automatic
effect replay, deletion of plaintext legacy copies, or secure-erasure promise.
