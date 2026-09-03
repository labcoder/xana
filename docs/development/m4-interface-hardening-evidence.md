# M4 interface hardening evidence

> Audience: Contributors and coding agents
>
> Authority: Verification record
>
> Scope: M4-23 implementation evidence; M4-24 owner and platform gates remain open

This record explains which Milestone 4 interface properties are enforced by
code and deterministic tests, which measurements can be reproduced locally,
and which results still require the owner's real displays, assistive
technologies, and supported operating systems. It is not a release record and
does not substitute projection time for paint or interaction evidence.

## Bounded data and scheduling paths

| Boundary | Enforced behavior | Evidence |
|---|---|---|
| Runtime to Desktop | A 256-item typed channel carries updates. A data-free coalescing signal wakes GPUI only after an update is queued. When presentation pauses, critical updates enter a bounded 64-item deferred queue; the runtime continues, priority receipts/stops are retained, and overflow requests authoritative resync. | `DesktopWakeSignal`; wake-without-polling, stalled-projection, and active-runtime regression fixtures |
| UI drain | One UI batch drains at most 64 updates. A saturated batch continues immediately so a coalesced wake cannot strand queued work. | `MAX_UPDATES_PER_FRAME`; Workbench driver |
| Settled Desktop | No 16 ms or other periodic runtime poll remains in Desktop presentation code. | Desktop dependency/authority gate rejects `UPDATE_INTERVAL` |
| Foreground executor | Settings-manager reads, configuration/account/model operations, maintenance work, and native path inspection execute on GPUI's background executor. Foreground entities receive only bounded typed results and perform presentation updates. | centralized `LoadedManager` path; strict all-target Clippy and Desktop tests |
| Durable history to snapshot | A recent contiguous suffix contains at most 512 messages and 2 MiB. Candidate messages are serialized once while constructing the suffix; no repeated front-removal occurs. | 10,000-message and byte-bound frontend fixtures |
| Progressive answer | Replaceable deltas may coalesce under pressure; authoritative final content converges by Operation identity. | 10,000-delta Desktop fixture and embedded slow-observer fixture |
| Artifact staging | Files are copied in 16 KiB chunks into create-only content-addressed storage with source identity and length rechecked after the read. | bounded file publication, deduplication, mutation, and over-limit fixtures |
| Inspection | Resource classification reads at most the smaller of 64 KiB and the configured in-memory limit. A valid source may exceed that probe limit without being buffered whole. | large WebM ingestion fixture |
| Raster previews | Only the newest eight eligible previews totaling at most 20 MiB of source data and an estimated 32 MiB of decoded RGBA data are admitted. Unknown dimensions and checked-arithmetic overflow stay typed cards. Snapshot replacement evicts entries outside that window. | count, source-byte, decoded-byte, overflow, missing-dimension, duplicate, eviction, and revisit fixtures |
| UI retention | Composer state is capped at 128 Conversations, each queue at 32 turns, recoverable submissions at 64, and retained GPUI Conversation entities at 128. | component/store fixtures and compiled bounds |
| Espejo | Eight hosted Conversations and four simultaneous mock streams remain grouped by stable identity; global notices retain their hard bound. | Espejo load fixtures |

The snapshot boundary intentionally leaves durable paging and advanced
compaction policy to M6. M4 proves that the local presentation does not require
all durable history in memory; it does not claim that 512 messages is the final
history-navigation product design.

## Reproducible release-profile record

Run from the repository root:

```bash
pwsh ./scripts/measure-m4-interface.ps1
```

The script builds the CLI/TUI and Desktop release binaries, runs the ignored
10,000-message snapshot and progressive-projection probes, and writes Markdown
plus JSON under `target/`. The JSON records OS, architecture, toolchain, binary
bytes, retained messages, projected bytes, batch size, and p95/p99 data-plane
timings. Generated evidence is deliberately not committed: each supported
reference system must produce its own record.

These probes measure bounded Xana data transformations. They do **not** measure
window readiness, GPUI paint, input-to-paint, refresh cadence, CPU, RSS/private
memory, GPU cache, power state, scale, IME, or assistive technology. Those facts
must be recorded separately with hardware, OS, GPU/backend, native or virtual
status, display refresh, power mode, scale, window size, method, sample count,
and noise notes.

One local Windows x64 release-profile sample on 2026-09-03 at Xana `5bc5690`
(Rust/Cargo 1.97.1) recorded a 39,193,600-byte CLI/TUI executable and a
66,094,080-byte Desktop executable. The 10,000-message snapshot retained 512
messages and 93,185 projected bytes at 2,880 microseconds p95 and 2,928
microseconds p99. The 10,000-delta Desktop probe retained 513 messages and
82,082 projected bytes, drained at most 64 updates per batch, and recorded 120
microseconds p95 and 183 microseconds p99. These are data-plane measurements
from one machine, not startup, paint, process-memory, GPU, or cross-platform
claims.

## Dependency and authority gate

`scripts/check-desktop-dependencies.ps1` proves all of the following without a
live provider or secret:

- the default root package resolves no GPUI dependency;
- the Desktop package has only its reviewed direct production dependencies;
- `gpui-ai`, `gpui-component`, and GPUI resolve to the reviewed coordinated pins;
- Desktop presentation source cannot spawn processes, open raw sockets, issue
  direct provider HTTP requests, or access the credential store; and
- Desktop presentation cannot regain the removed periodic polling clock.

Provider, tool, artifact, path, credential, and process authority stays behind
typed `xana::desktop` adapters. Model-authored Markdown loses remote images,
HTML/MDX, credential-bearing URLs, fragments, and non-HTTP(S) schemes before it
reaches clickable presentation. Native GPUI has no browser DOM, WebView bridge,
or general renderer IPC; local web remains deferred.

## Accessibility gates

Automated tests cover palette contrast, bounded 100-200% text scale, semantic
heading/status/dialog roles, stable keyboard command identity, reduced-motion
preference projection, pseudolocalized and representative Spanish semantic
copy, and inspectable fallback for missing or unknown semantic codes. The
component catalog uses the same pinned components and visual globals as the
Workbench.

Automation cannot certify the owner experience. M4-24 still requires complete
keyboard paths, focus order and restoration, text selection, IME composition,
200% reflow, reduced motion, no-color/plain terminal behavior, and hands-on
NVDA, VoiceOver, and Orca (or a documented Linux equivalent) checks.

## Resource and media limits

Tests exercise declared/detected media disagreement, checked aggregate
accounting, zero/unlimited rejection, per-kind soft limits beneath immutable
ceilings, oversized metadata, external-path approval, missing artifacts,
content digest verification, and safe static-raster decoding. SVG and Lottie
remain pending active-document types; animated raster, audio, video, binary,
unknown, rejected, and unavailable resources retain typed non-executable cards
and explicit external actions. M4 does not claim native audio/video playback,
active SVG, native Lottie, arbitrary codecs, or remote media autoload.

Decoded-memory/GPU behavior, repeated mount/unmount, animation cadence, and
eviction/revisit feel remain reference-system checks because the pinned GPUI
renderer and platform backend own those effects. The application also admits
no more than an estimated 32 MiB of RGBA pixels at once. That estimate is a
conservative deterministic gate, not a claim about the renderer's actual CPU or
GPU allocation; the source-byte and decoded-byte bounds measure different
risks.

## Verification commands

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-targets --no-default-features
pwsh ./scripts/check-desktop-dependencies.ps1
pwsh ./scripts/measure-m4-interface.ps1
```

CI repeats deterministic checks on Windows x64, macOS ARM64, macOS Intel, and
Linux x64 glibc as defined by the workflow. Cross-platform CI and the generated
reference-system records remain pending until this unpushed work is exercised
there.

## Owner/reference-system evidence still required

- warm and cold interactive startup, input and runtime-event-to-paint latency;
- 60 Hz p95/p99 and native 120+ Hz evidence where hardware is available;
- settled redraw, idle CPU, private memory/RSS, decoded-image/GPU-cache return;
- sustained scrolling/streaming, repeated layout and media mount/unmount, and
  crash/reconnect behavior;
- keyboard, IME, 200% scale/reflow, reduced motion, selection/copy, and all
  required screen-reader paths; and
- Windows x64, macOS ARM64, macOS Intel, and Linux x64 glibc source-build and
  lifecycle results.

Missing owner or platform evidence is an open gate, not a passing result.
