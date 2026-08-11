# Xana 0.5.0 Release Preview evidence

> Historical topology note: this evidence predates the single-package
> consolidation. Commands naming `xana-core` or `crates/xana-cli` record what
> was actually measured then; current commands live in
> [Release Preview development](release-preview.md).

> Status: Local candidate verified; remote four-target gate not yet executed
>
> Publication: Not authorized, not tagged, no GitHub draft, no public release

This record separates completed repository evidence from external release
effects. The locally tested code candidate is commit `565e75d` plus this
evidence-only documentation change. The release workflow will record the exact
final commit for any no-publish bundle or tag; no commit, local version, or
document is treated as a release by itself.

## Local reference environment

- Date: 2026-08-09 America/Los_Angeles
- Host: Windows x64, native MSVC target
- Rust: pinned 1.97.1
- Release planner: cargo-dist 0.32.0, verified installer digest
  `b657cf8c04a8b7bc28f39d220f7e6dd11bbd2bdb072c552262bd9ccf597261b5`
- Candidate version: `xana 0.5.0`

No live provider credential, Codex account, private Xana config, transcript, or
owner state was used by the Release Preview tests.

## Passed local gates

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | Passed |
| warning-denied workspace Clippy, all targets/features | Passed |
| all-feature workspace tests | 521 passed, 5 documented live/manual tests ignored; 17 process tests passed |
| no-default-feature workspace tests | 520 passed, 5 documented live/manual tests ignored; 17 process tests passed |
| `cargo package --package xana-core --locked --allow-dirty` | 5 files packaged and verified; dirty flag was required only because the local version candidate preceded its commit |
| root package-path audit | 224 paths, required public docs/installers present, forbidden state absent |
| pinned release plan | One Xana app, exact four native targets, conventional archives, SHA-256, native runners, attestation intent |
| Bash installer matrix | Install/reinstall, exact version, quoted custom path, idempotent PATH, setup pending, corrupt/unexpected archive, unsupported target, rollback |
| PowerShell installer matrix | Install/reinstall, exact version, isolated PATH, setup pending, corrupt/extra ZIP, bad executable, locked destination, PATH rollback |
| release workflow/bundle audit | Immutable actions, least privilege, manual-main restriction, exact fifteen-asset assembly, mismatch rejection, draft-only boundary |
| installation/removal docs | Channels, targets, flags, links, trust limits, no bypass recipe; executable/PATH-only removal preserves personal state |

The PowerShell one-line invocation was also exercised as an in-memory script
block. Help and failure return to the invoking shell rather than terminating
the user's PowerShell session.

## Windows native artifact and source-channel measurements

Measurements are observational, not wall-clock CI budgets:

| Method | Time | Archive/binary | Temporary file bytes | Cleanup |
|---|---:|---:|---:|---|
| cargo-dist 0.32.0 native Windows build | 48,952 ms | ZIP 7,238,659 bytes; staged `xana.exe` 18,756,096 bytes | Planner build tree not treated as installer temp | Archive audit staging removed |
| Real PowerShell fixture first install (debug fixture) | 3,606 ms | ZIP 10,683,984 bytes; binary 36,590,592 bytes | 47,275,176 bytes at the manifest/archive/staged-binary file peak | `cleanup_residue=0` |
| Locked local `cargo install --path crates/xana-cli` | 44,261 ms | installed release binary 18,546,176 bytes | Cargo-owned build tree, not installer temp | isolated install prefix removed, residue 0 |

The different Cargo and dist executable sizes do not justify profile tuning:
both passed exact version/help execution, and Release Preview deliberately
inherits Cargo's release profile. Cross-platform measurements must be compared
before any later size optimization.

## Evidence still required from the remote gate

The repository does not currently contain honest evidence for these external
facts:

- CI success for this candidate on Windows, macOS ARM64, macOS Intel, and Linux
  x64 glibc;
- a manual-main no-publish workflow bundle containing the four real native
  archives and GitHub provenance attestations;
- clean and prior-version archive/installer smokes on all four native targets;
- independent `gh attestation verify` results, per-platform size/time/temp
  measurements, and the completed human draft checklist; or
- an exact release tag, unpublished GitHub draft, owner publication decision,
  or public release URL.

These are not substituted with fixture results. The next authorized sequence
is: commit and push the candidate; let ordinary CI pass; explicitly dispatch
the no-publish workflow from `main`; verify its bundle and attestations on the
four target environments; then ask the owner whether to create the exact tag.
A tag may assemble an unpublished draft, but publication remains a later human
action. If any remote gate fails, the owning RP ticket reopens and no release is
accepted.

## Product Distribution deferrals

Release Preview still has no platform signing/notarization, package-manager
channel, `xana update`, background checks, signed update metadata, retained
rollback, uninstall/purge command, broader ARM64/musl matrix, crates.io
publication, desktop distribution, or support SLA. A correction after any
future publication must use a new patch version rather than replacing a tag or
asset.
