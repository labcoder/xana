# Release Preview development

> Audience: Contributors and release reviewers

Xana uses `cargo-dist` 0.32.0 as a pinned planner for one terminal application
and four native targets. The reviewed [Release Preview proposal](../proposals/0018-release-preview-distribution.md)
owns the product boundary. This document describes local implementation tools;
it does not claim that a public archive or installer exists.

## Plan the release

Install exactly `cargo-dist` 0.32.0, available as the `dist` executable, then
run:

```powershell
./scripts/check-release-plan.ps1
```

The check derives `v<workspace-version>` from `Cargo.toml`, invokes the real
planner, and semantically verifies:

- one application, the `xana` binary from package `xana-cli`;
- macOS ARM64, macOS Intel, Windows x64 MSVC, and Linux x64 glibc only;
- `.tar.gz` Unix archives and one Windows `.zip`;
- `LICENSE`, `README.md`, `installation.md`, and the executable only;
- SHA-256 sidecars and a unified checksum file;
- GitHub attestation intent and one native runner per target; and
- absence of machine-local workspace paths.

`dist-workspace.toml` leaves generated installers disabled. Xana's Bash and
PowerShell wrappers are separately reviewed product code; the release workflow
attaches those exact source files later.

## Build and audit the current target

Build the current native archive using the target triple for the current host:

```powershell
dist build --artifacts=local --target x86_64-pc-windows-msvc
./scripts/check-release-archive.ps1 \
  -Archive target/distrib/xana-cli-x86_64-pc-windows-msvc.zip \
  -Target x86_64-pc-windows-msvc
```

Use `aarch64-apple-darwin`, `x86_64-apple-darwin`, or
`x86_64-unknown-linux-gnu` on the corresponding native host. The archive audit
verifies the SHA-256 sidecar, rejects unsafe or unexpected names, extracts into
an isolated temporary directory, and executes `xana --version` and `xana
--help`. It never publishes, uploads, tags, or installs the artifact.

Release archives intentionally retain Cargo's existing release profile. The
preview has no measured justification for custom LTO, stripping, panic, or
codegen settings. `publish = false` and the unpublished workspace package
relationships remain unchanged; prebuilt distribution is not crates.io
publication.
