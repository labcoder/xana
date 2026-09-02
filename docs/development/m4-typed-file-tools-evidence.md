# M4 typed file and search tool evidence

> Audience: Contributors and coding agents
> Authority: Descriptive

This record validates the M4-03B native typed-tool baseline implemented by
`2a0c743` and documented by `53c0b84`. It records bounded facts and commands,
not workspace contents or user data.

## Contract exercised

The production capability snapshot exposes nine deterministic built-ins:
`read_file`, `list_files`, `find_files`, `grep_files`, `write_file`,
`edit_file`, `run_command`, `read_document`, and `xana_docs`.

The tests exercise:

- immutable provider schema serialization, effect class, replay declaration,
  normalized final arguments, correlated call ids, and JSON output;
- one typed create -> discover -> grep -> atomic multi-edit -> paged-read
  workflow without shell file-writing;
- workspace and exact external-file permission scopes, denial-before-effect,
  race revalidation, symlink escape rejection, and observer-independent
  execution;
- durable permission, intent, result, named-output, crash-prefix, contract
  match, and explicit replay behavior through the tool-neutral operation
  executor;
- invalid UTF-8, binary input, oversized input/output, hostile glob/regex,
  nested `.gitignore`, changed targets, edit overlap/count mismatch, and
  create/overwrite preconditions; and
- independent command streams, nonzero exit, platform shell behavior,
  immutable timeout bounds, and kill-on-drop timeout behavior.

Recursive discovery/search runs on Tokio's blocking pool. Dropping the async
invocation does not retain results or block the runtime; a read-only worker may
finish its already bounded traversal. `run_command` owns a kill-on-drop child,
so timeout or owner cancellation stops the process rather than detaching it.

## Resource observations

Measured on Windows x86-64 on 2026-09-01 with the repository-pinned Rust
toolchain and lockfile:

| Probe | Observation |
|---|---|
| Complete prompt baseline | 6,008 system bytes / 2,002 estimated tokens |
| Nine provider tool schemas | 7,113 bytes / 2,374 estimated tokens |
| Wide `find_files` fixture | 41 entries visited, 40 returned, 2,273 encoded output bytes, explicit result-limit truncation |
| Large `grep_files` fixture | 400 approximately 1 KiB matching lines; encoded result remained at or below 64 KiB and reported output truncation |
| Hung command fixture | A five-second command stopped at its reviewed 25 ms timeout; focused test completed in approximately 30 ms |

Immutable production ceilings are 50,000 visited entries, two seconds of
walker time, 64 KiB encoded discovery/search output, 2 MiB per searched file,
16 MiB total searched bytes, and 120 seconds per command. Search overflow has
no hidden full result or artifact: the result reports the reached bound and
requires a narrower query. This keeps unselected output out of prompt,
session projection, and frontend memory.

## Verification

The required local gate passed:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo doc --locked --no-deps --all-features
```

The full test run passed 875 library tests with 6 intentionally ignored manual
probes, 20 CLI integration tests, and 4 settings integration tests. The focused
durability suite passed 9 operation tests, and the correlated multi-round
agent test passed. The focused typed-tool suite passed 83 tests.

Windows-specific shell and timeout fixtures ran locally. Unix-specific
symlink and POSIX-shell fixtures are compiled and exercised by the existing
Linux/macOS CI jobs on the next integration push; this repository change does
not claim that an unpublished CI run has already occurred.
