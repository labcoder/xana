# M4 state migration and ownership evidence

> Audience: Xana maintainers  
> Authority: Development evidence  
> Scope: M4-07A

This record explains the implemented migration and collision boundary. Current
Architecture and User Documentation remain authoritative for shipped behavior.

## Implemented boundary

- `WorkspaceIdentity` opens and canonicalizes the workspace once, hashes its
  filesystem identity, and uses that key for both embedded workspace-root and
  foreground local-host collision domains. Path spelling is display data, not
  authority.
- A successful OS-lock claim advances a durable generation. Descriptors,
  client hello frames, and initial host snapshots must agree on the exact
  generation. Stale descriptors and PIDs remain diagnostic only.
- Foreground discovery has an explicit `Owned` or `Attach` claim result. A
  losing claimant receives the compatible lock-backed owner descriptor rather
  than constructing a second owner.
- The seven `data/interoperable/` records now write version 2. Version 1 is a
  typed migration input; future, malformed, oversized, or changed records fail
  before migration mutation.
- Every interoperable-state mutation shares one global cross-process lock.
  Migration snapshots reviewed bytes, retains exact backups, writes a bounded
  journal, installs records atomically, and commits config last.
- Recovery uses the journal's source and target config digests. Source config
  rolls private records back byte for byte; target config validates and
  finalizes forward. Neither path replays work.

## Deliberate state ownership

Private-record version 2 changes the transactional envelope, not the domain
payload. M4-07A does not duplicate these facts:

| Fact | Existing owner |
|---|---|
| Foreground host/controller generation | Runtime lock and protected descriptor |
| Run recovery and committed effects | Native session/operation owner or managed runtime |
| Attention | Derived from Runs, approvals, failures, and unread state |
| Conversation lineage and Project placement | Private Project registry |
| Provider-owned history | Provider/managed runtime |
| Usage observations | Deferred to the typed usage implementation that owns provenance and deduplication |

This keeps recovery deterministic and avoids two durable authorities for one
fact.

## Failure matrix

| Case | Expected result | Evidence |
|---|---|---|
| Dotted, case, Windows-prefix, or Unix-symlink alias | Same collision key and one lock domain | `workspace_identity` platform-gated tests |
| Simultaneous foreground claims | First owns; second attaches to the exact generation | `local_host::descriptor` claim test |
| Owner release and restart | New generation is greater; stale descriptor grants nothing | local-host and workspace-host generation tests |
| Wrong protocol/host/generation/workspace/capability | Rejected before snapshot authority | local-host authentication tests |
| Real v1 records with Project relations | All seven migrate; membership, profile snapshot, and predecessor survive | private migration v1 fixture |
| Mixed current, v1, and missing records | Exact migrate/initialize counts and all targets become v2 | mixed-record fixture |
| Source changes after review | Failure before journal or target mutation | changed-source fixture |
| Injected failure after three writes | Every source restored byte for byte; backup retained | partial-write fault fixture |
| Crash before config commit | Mutation stays blocked; explicit retry rolls back then reapplies | pre-commit recovery fixture |
| Crash after config commit | Retry validates forward and never downgrades | post-commit recovery fixture |
| Held migration/store lock | Typed busy result without mutation | global-lock fixture |
| Pending journal plus ordinary update | Update refuses with exact recovery action | mutation-gate fixture |
| Unrelated Run/session sentinel | Untouched across migration | ownership fixture |

## Verification

The implementation was checked with:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test workspace_identity --all-features
cargo test local_host --all-features
cargo test workspace_host --all-features
cargo test private_state --all-features
cargo test config_migration --all-features
cargo test doctor --all-features
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

Both complete workspace test modes passed locally on Windows: 910 library
tests, 20 CLI integration tests, four settings integration tests, and three
Desktop projection tests passed in each mode; six explicitly manual probes
remained ignored. Cross-platform CI remains the release gate rather than being
inferred from one development machine.

## Commits

- `49a9840` — filesystem collision identity, generation-backed claims, and
  attach-or-own protocol enforcement.
- `7717889` — private-record v2 transaction, retained backups, rollback, and
  crash recovery.
