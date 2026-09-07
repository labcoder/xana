# Native protected-storage dependencies

> Audience: Contributors and coding agents
> Authority: Descriptive build contract

The Windows-first production dependency choice is SQLCipher **4.18.0**
(SQLite **3.53.4**) through rusqlite **0.40.2**, with statically built OpenSSL
**3.6.3**. Standard **age 0.12.1** envelopes provide independent recovery and
streaming artifact encryption. OS custody uses platform-specific keyring stores;
Windows storage keys use **Local**, not Enterprise/roaming, persistence.
Selection does not mean the whole application has migrated to protected storage.

## Reproducibility

`Cargo.lock` fixes the Rust/native source versions. The published
`libsqlite3-sys` 0.38.2 embeds SQLCipher 4.14.0, so the workspace patches it with
the reviewed upstream 4.18.0 amalgamation. The small build adjustment, exact
commits, generation command and upgrade procedure are in
[the vendored dependency record](../../vendor/libsqlite3-sys/README.xana.md).
`tests/encrypted_storage_contract.rs` checks source SHA-256, the **actually
linked** cipher/SQLite versions, wrong-key rejection and encrypted WAL/FTS
canaries. Do not weaken that test to make a system-library override pass.

Use `cargo … --locked` from this Git workspace. `cargo install --path .` and
Git installation retain the patch. A normalized `.crate` drops it and is **not
a supported Xana distribution**. CI verifies the locked source installation
instead; native release builds use the same repository workspace and pins.

On Windows, a complete Perl distribution and NASM 3.02 are required alongside
MSVC. `OPENSSL_SRC_PERL` can select Perl explicitly; `.cargo/config.toml`
requires assembly support rather than silently accepting a no-asm build. The
CI helper verifies the NASM archive SHA-256 before using it, installs nothing
system-wide and changes no execution policy. The matching OpenSSL static PDB
is copied beside Cargo's rlib to retain debugging information without LNK4099.

## Artifact choice and remaining validation

Use transactional encrypted rows for metadata, records and indexes; large
artifact payloads use standard age streams. A range reader must authenticate
the complete object, declared length and expected BLAKE3 identity, then return
only the bounded requested range. Chunk-only authentication is not equivalent
to Xana's existing immutable-object guarantee. Do not retain one open BLOB
handle per artifact or buffer the complete payload merely to return a range.

Windows feasibility tests covered real custody, independent recovery,
interrupted writes and bounded full-object artifact verification. Native macOS
ARM64/Intel and Linux glibc custody, lifecycle, packaging and resource evidence
remain release-acceptance requirements, not Windows implementation blockers.
Missing native runs are not passes. The accepted
[storage contract](../proposals/0024-encrypted-managed-content-and-recovery.md)
continues to govern migration, lock and no-plaintext-fallback behavior.

## Opt-in native custody check

The [on-demand native workflow](native-qualification.md) provisions disposable
GitHub-hosted Linux/macOS environments and runs this exact fixture without
requiring access to the owner's machines or accounts. Workflow success covers
the named automated checks, not interactive OS prompts or browser parity.

Normal tests use disposable in-memory custody and never access the OS key store.
An ignored production-seam test is available for an explicitly authorized native
check in an ordinary logged-in user session:

```text
cargo test --offline --locked -p xana --lib --all-features storage::keys::native_tests::production_os_custody_unlock_loss_lock_and_independent_recovery -- --ignored --exact --nocapture
```

It creates one UUID-named credential under `dev.xana.protected-storage` and one
synthetic temporary encrypted home. It rejects an existing credential at that
identity, verifies reopen, explicit lock/unlock, missing-key refusal and recovery
without that credential, then verifies exact credential and directory cleanup.
It neither enumerates nor reads existing credentials and never opens the normal
Xana home. A cleanup failure fails the test and names only the fixture identity;
do not delete the whole service or key store to recover from a failed fixture.

This check alone does not establish denied/locked-service prompt behavior,
native Desktop interaction, browser ownership or resource measurements. Record
those separately on each supported native target, at the same source revision.
Cross-compilation and a root WSL session without Secret Service are not native
custody evidence. Missing tools or cached sources are prerequisites, not a reason
to install software or change the key-store policy implicitly.
