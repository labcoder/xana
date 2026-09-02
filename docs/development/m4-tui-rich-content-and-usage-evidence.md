# M4 TUI rich-content, resource, and usage evidence

> Status: Implementation complete; owner terminal verification pending
> Scope: M4-11

## Implemented boundary

- The TUI renders every shared semantic content variant through a bounded rich,
  text, metadata, or unsupported fallback. Model content is sanitized before it
  reaches Ratatui.
- Inline images use `appearance.inline_image = auto|off`. `auto` requires a
  positively detected terminal protocol and usable dimensions; an unproven
  multiplexer, failed probe/decode, resize, or `off` keeps the metadata fallback.
- Artifact preview, reference copy/insertion, save, reveal, and open are explicit
  actions. Rendering never opens a path or application.
- `/attach PATH|--clipboard|list|clear` stages bounded immutable resources.
  Workspace paths use workspace authority; external files require one exact
  allow-once decision before I/O. Declared and detected media types remain
  distinct.
- PNG/JPEG/GIF input crosses a provider boundary only for an exact route/model
  that advertises image input. Other recognized media remains staged with
  metadata and artifact actions and fails closed before disclosure.
- `/usage compact` and `/usage details` render the shared semantic usage,
  prompt-plan, execution, completion, and surface-capability facts. Native
  deltas and managed cumulative snapshots retain their accounting semantics and
  unavailable values never become zero.

## Automated evidence

On Windows, after the implementation and documentation changes:

- `cargo fmt --all --check` passed.
- `cargo clippy --all-targets --all-features -- -D warnings` passed.
- `cargo test --all-targets --all-features` passed: 1,040 library tests, 20 CLI
  tests, and 4 settings CLI tests; 6 explicit manual/timing fixtures remained
  ignored.
- Focused fixtures cover terminal protocol/multiplexer detection, bounded
  previews and restore, malicious rich text, 10,000 projected messages,
  external-resource approval, WebM/MP3 classification, attachment clearing,
  usage observation deduplication, managed cumulative replacement, and honest
  unknown/provenance rendering.

## Owner verification still required

Exercise inline-image auto detection, resize and terminal restore on Windows
Terminal/PowerShell, macOS, Linux, and one multiplexer; confirm metadata
fallback in an unsupported terminal and with `inline_image = off`. Also review
keyboard/screen-reader/plain, ASCII/no-color, reduced-motion, and resource-card
presentation. These checks are visual or environment-specific and are not
claimed by deterministic CI.

## Commits

- `9f2edfe` — shared semantic content rendering
- `843ae07` — safe artifact and inline-image previews
- `04d8ced` — scoped semantic usage presentation
- `32be6c9` — typed local-resource staging
- `2c9ef2c` — user and architecture documentation
