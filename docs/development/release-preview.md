# Release Preview development

> Audience: Contributors and release reviewers

Xana uses `cargo-dist` 0.32.0 as a pinned planner for one terminal application
and four native targets. The reviewed [Release Preview proposal](../proposals/0018-release-preview-distribution.md)
owns the product boundary. This document describes local implementation tools;
it does not claim that a public archive or published installer asset exists.

## Plan the release

Install exactly `cargo-dist` 0.32.0, available as the `dist` executable, then
run:

```powershell
./scripts/check-release-plan.ps1
```

The check derives `v<workspace-version>` from `Cargo.toml`, invokes the real
planner, and semantically verifies:

- one application, the `xana` binary from package `xana`;
- macOS ARM64, macOS Intel, Windows x64 MSVC, and Linux x64 glibc only;
- `.tar.gz` Unix archives with the canonical `xana-<target>/` cargo-dist root
  and one flat Windows `.zip`;
- `LICENSE`, `README.md`, `installation.md`, and the executable only beneath
  that platform layout;
- SHA-256 sidecars and a unified checksum file;
- GitHub attestation intent and one native runner per target; and
- absence of machine-local workspace paths.

`dist-workspace.toml` leaves generated installers disabled. Xana's Bash and
PowerShell wrappers are separately reviewed product code; the release workflow
attaches those exact source files later. Xana also owns a stricter
no-publish/draft-only workflow than cargo-dist generates, so `allow-dirty =
["ci"]` disables only cargo-dist's generated-CI freshness check. The checked-in
workflow remains covered by `check-release-workflow.ps1` and release-bundle
fixtures; cargo-dist still owns planning and native archive construction.

## Audit the Bash installer

The reviewed Unix wrapper lives at `install/install.sh`. It supports `latest`
or one exact `X.Y.Z`, an explicit install directory, `--no-setup`, and explicit
`--modify-path`/`--no-modify-path` behavior. Production authority is fixed to
HTTPS assets in `labcoder/xana`; the offline fixture seam requires
`--allow-test-fixture`, `--test-fixture-root`, and `--test-target` together and
prints a test-only warning.

Run its syntax and offline behavior matrix from Bash:

```bash
bash -n install/install.sh scripts/test-install-sh.sh scripts/verify-sha256.sh
bash ./scripts/test-install-sh.sh
bash ./scripts/test-release-sha256.sh
```

The matrix covers exact-version install and reinstall, cargo-dist's canonical
rooted Unix archive layout, idempotent profile editing, noninteractive
setup-pending translation, checksum mismatch, unexpected archive inventory,
unsupported targets, and preservation of the previous executable. The
checked-in release manifest generator is:

```powershell
./scripts/new-release-manifest.ps1 \
  -ArtifactDirectory target/distrib \
  -Version 0.5.1
```

It accepts only the exact four archive names, hashes their bytes itself, bounds
their sizes, and writes the line-oriented `xana-release-manifest.txt` consumed
equivalently by both installers. The draft workflow will own invoking it only
after all four native artifacts have been assembled.

## Audit the PowerShell installer

The reviewed Windows x64 wrapper lives at `install/install.ps1` and exposes the
same latest/exact-version, custom-directory, setup, and explicit PATH choices.
It does not request elevation or require an execution-policy change. Its
fixture authority likewise requires explicit local root and target switches;
the optional test user-PATH file prevents the offline matrix from touching the
real user environment.

Run its offline matrix from PowerShell:

```powershell
./scripts/test-install-ps1.ps1
```

The matrix rebuilds the current debug binary, then covers install/reinstall,
exact release binding, idempotent isolated PATH updates, noninteractive setup
pending, corrupt and unexpected ZIPs, staged smoke failure, locked destination,
and PATH failure rollback. CI runs the Bash matrix on every platform and this
native PowerShell matrix on Windows.

## Build a no-publish bundle

`.github/workflows/release.yml` has two entry paths. Its release-builder
downloads are checked by a source-controlled portable SHA-256 verifier that
does not assume GNU `sha256sum` options on macOS. Manual dispatch accepts an
exact workspace version and requires that the exact source commit already has
a successful ordinary CI push run on `main`. It then builds the four targets
natively, assembles and attests the exact bundle, and retains it as a workflow
artifact. Manual dispatch has no draft-creation job. An exact matching
`vX.Y.Z` tag performs the same work, then gives only the final job `contents:
write` so it can leave an unpublished GitHub draft.

Ordinary CI runs on pull requests and pushes to `main`, but not tag pushes. It
uses a commit-pinned Rust cache that retains dependency build artifacts while
excluding Xana workspace outputs and Cargo-installed binaries; only trusted
`main` pushes save cache entries. Superseded runs are cancelled. The Release
Preview does not consume those cached outputs or CI binaries: all public
archives are fresh native builds of the evidenced commit. Every job has an
explicit timeout, intermediate release-plan/native artifacts expire after one
day, and only the complete review bundle is retained for fourteen days.

The workflow uses immutable action commits, verified cargo-dist 0.32.0, current
standard `macos-15` ARM64 and `macos-15-intel` runners, and fixed Windows/Linux
runners. Run the local authority and assembly checks before any remote run:

```powershell
./scripts/check-release-workflow.ps1
./scripts/test-release-ci-evidence.ps1
./scripts/test-release-archive-contract.ps1
./scripts/test-release-bundle.ps1
```

An assembled bundle has exactly fifteen assets: four archives, four archive
checksum sidecars, `dist-manifest.json`, `xana-release-manifest.txt`, the two
source-controlled installers, versioned release notes, the reviewer checklist,
and `sha256.sum`. Missing, duplicate, mismatched, or extra inputs fail before
upload. A tag job initially labels the draft `INCOMPLETE`; only exact remote
inventory and tag-commit verification changes the title to `REVIEW READY`.
Neither title publishes the release.

The owner follows [the draft review checklist](release-review-checklist.md),
including independent GitHub attestation verification, before a separate
manual publish action. Published tags and assets are immutable by policy; a
correction uses a new patch version.

## Build and audit the current target

Build the current native archive using the target triple for the current host:

```powershell
dist build --artifacts=local --target x86_64-pc-windows-msvc
./scripts/check-release-archive.ps1 \
  -Archive target/distrib/xana-x86_64-pc-windows-msvc.zip \
  -Target x86_64-pc-windows-msvc
```

Use `aarch64-apple-darwin`, `x86_64-apple-darwin`, or
`x86_64-unknown-linux-gnu` on the corresponding native host. The archive audit
verifies the SHA-256 sidecar, rejects unsafe or unexpected names, extracts into
an isolated temporary directory, and executes `xana --version` and `xana
--help`. It never publishes, uploads, tags, or installs the artifact.

Release archives intentionally retain Cargo's existing release profile. The
preview has no measured justification for custom LTO, stripping, panic, or
codegen settings. `publish = false` prevents registry publication; prebuilt
distribution is not crates.io publication.
