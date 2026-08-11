# Xana 0.5.0 architecture closure evidence

> Audience: Contributors and release reviewers  
> Status: Local architecture closure verified; remote CI still required

This record closes the local evidence for the Milestone 2 package and naming
simplification. It does not claim that the candidate is tagged, uploaded,
drafted, or published.

## Resulting shape

- Cargo metadata reports one workspace member and one application package,
  `xana`, containing the library entry used by the executable, the `xana`
  binary, and process-level integration tests.
- The package's internal modules are private. The only public Rust item is
  `xana::entry`, required by the package's separate binary target. Xana does
  not promise a public Rust SDK.
- `native_runtime` owns Xana's native foreground loop and durable native
  conversation state. `managed_execution` adapts a foreign inner loop for
  Xana clients. `managed` owns the foreign transport and account/catalog RPC.
- `plain_terminal` and `tui` are interaction surfaces. `frontend` is the
  repository-private command/event/snapshot seam used by embedded and
  loopback clients. Frontends do not own provider, permission, orchestration,
  or persistence policy.
- `model_catalog` owns connection-scoped discovery and selection; `tui/state`
  owns only full-screen view state and transitions.

No compatibility crate, obsolete module alias, or second execution policy was
retained. Proposal 0001 leaves future package names open. A public engine or
frontend crate waits for a second real frontend to prove the smallest reusable
boundary.

## Adversarial findings resolved

The closure review found two issues hidden by the former topology:

1. Public module visibility suppressed dead-code diagnostics and made internal
   capability, document, and self-documentation types look like an accidental
   SDK. The modules are now private, unused speculative variants/accessors were
   removed, feature-only document limits compile only with that feature, and
   useful documentation audience/authority metadata is returned by the
   existing catalog listing.
2. After package consolidation, cargo-dist excluded the package because
   crates.io publication is disabled. The package now opts into prebuilt
   distribution explicitly with `package.metadata.dist.dist = true` while
   retaining `publish = false`.
3. Cargo-dist's generated-CI freshness check attempted to replace Xana's
   deliberately stricter no-publish/draft-only workflow. The configuration now
   excludes only generated CI from cargo-dist ownership; repository checks and
   fixtures continue to validate the source-controlled workflow while
   cargo-dist remains the planner and native archive builder.

The capability resolver also now rejects a tool whose declared capability is
absent instead of silently dropping that malformed contribution. The existing
duplicate, dependency, selection, permission, cancellation, bounded-memory,
credential, path, and redaction contracts remain covered by their focused
tests.

## Local verification

The following checks passed on Windows x64 with Rust 1.97.1:

| Gate | Result |
|---|---|
| `cargo metadata --no-deps --format-version 1` | One package and one workspace member |
| `cargo fmt --all --check` | Passed |
| warning-denied Clippy, all targets and all features | Passed |
| warning-denied Clippy, all targets and no default features | Passed |
| all-feature tests | 550 passed, 5 documented manual/live tests ignored; 17 process tests passed |
| no-default-feature tests | 549 passed, 5 documented manual/live tests ignored; 17 process tests passed |
| verified cargo-dist 0.32.0 release plan | One application, four native targets, SHA-256, attestations |
| release workflow and bundle fixtures | Exact inventory, immutable actions, least privilege, draft-only boundary |
| installation/removal documentation checks | Passed |
| clean source-package audit | 243 paths; required public files present and runtime state absent |
| Bash installer fixture | Install/reinstall, exact version, PATH, setup-pending, tamper/inventory, unsupported target, rollback passed |
| PowerShell installer fixture | Equivalent Windows matrix passed; first install 3,812 ms, 10,802,355-byte fixture archive, zero cleanup residue |
| native Windows cargo-dist archive | 7,363,096-byte ZIP; checksum, inventory, `--version`, and `--help` passed |

The remote platform matrix remains a separate release gate. A local pass cannot
substitute for Windows, macOS ARM64, macOS Intel, and Linux CI from the exact
pushed candidate.

## Intentionally deferred

- Public SDK/protocol stabilization and physical crate extraction wait for a
  second real frontend.
- Desktop, web, mobile, daemon, remote-host, multi-user, retained-work, signing,
  package-manager, and updater work remain in their existing future scopes.
- Tagging, draft creation, upload, and publication require the separate
  release-preview workflow and explicit owner authorization.

Waiting keeps current module seams inexpensive to change and avoids making
speculative package or compatibility promises before another consumer exists.
