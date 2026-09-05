# Xana's reviewed native dependency

This is upstream `libsqlite3-sys` 0.38.2 from rusqlite 0.40.2, commit
`e88f112bef7899234a497baed5cc3c3d553deeb8`. Keep its MIT license, build logic,
bindings and unrelated SQLite source intact. This is not a new crypto provider.

Two deliberate changes:

1. Replace `sqlcipher/sqlite3.c`, `sqlite3.h` and `sqlite3ext.h` with the official
   SQLCipher 4.18.0 amalgamation (SQLite 3.53.4), generated from upstream commit
   `63697beb0fafcb61faa7a3e6fd267036548ab11b` using
   `nmake /f Makefile.msc sqlite3.c`. Preserve `sqlcipher/LICENSE` (BSD-3-Clause).
2. On Windows MSVC, copy the matching vendored OpenSSL static debug PDB into
   Cargo's profile `deps` directory alongside the rlib. Without it the linker
   discards crypto debug information and emits LNK4099. No warnings are hidden.

The lockfile pins `openssl-sys` 0.9.117 and `openssl-src` 300.6.1+3.6.3
(OpenSSL 3.6.3, Apache-2.0). Build requirements are a C compiler, make and Perl;
Windows additionally needs a complete Perl installation and NASM for the
reviewed assembly-optimized build (`OPENSSL_RUST_USE_NASM=1`). Do not set
`OPENSSL_NO_VENDOR`, `SQLCIPHER_LIB_DIR`, or similar system-library overrides
when reproducing the reviewed build.

## Updating

Update the explicit upstream revisions, regenerate rather than hand-edit the
amalgamation, preserve licenses, and review the build/FFI diff. Update the
version/hash contract in `tests/encrypted_storage_contract.rs` only after
native storage, tamper, crash/recovery and range-performance tests pass. A
successful compile alone is not evidence of encryption or safe upgrades.
Run the full workspace test/Clippy matrix and native-platform acceptance before
shipping. Remove this patch when the published wrapper includes the accepted
cipher and equivalent Windows debug-symbol handling.
