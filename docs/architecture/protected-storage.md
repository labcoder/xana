# Protected managed storage

> Audience: Contributors and coding agents  
> Authority: Descriptive

`storage::ProtectedStore` owns one SQLCipher connection and zeroizing secret
bundle behind an `Arc<Mutex<Option<Database>>>`. Clones share revocation, not a
process-global key cache. The composition edge selects OS custody or the explicit
recovery-file environment setting. `Agent` remains unaware of either.

```mermaid
flowchart TD
    APP[Application / Desktop control plane] --> CUSTODY[OS custody or independent age recovery]
    CUSTODY --> STORE[ProtectedStore: revocable connection owner]
    SESSION[DurableSession / existing reducer] --> STORE
    RECORDS[Private records / managed handles / composer history] --> STORE
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
