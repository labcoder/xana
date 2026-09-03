# M4 adversarial closeout evidence

> Audience: Maintainers and coding agents
>
> Authority: Verification record
>
> Status: Automated adversarial review complete; owner and cross-platform gates remain open

This record separates the automated Milestone 4 closeout work from evidence
that only the owner or a supported target can supply. It does not mark M4
complete, change a proposal lifecycle, authorize a release, or replace the
manual acceptance record.

## Review scope

The review compared the implemented runtime, private frontend protocol, CLI,
plain mode, TUI, Desktop, tests, package boundaries, Architecture, accepted
Proposal 0022, ADR 0003, and M4 evidence. It challenged authority ownership,
attach-or-own behavior, command truthfulness, backpressure, bounded memory,
foreground responsiveness, dependency direction, accessibility fallbacks, and
future-scope leakage.

## Findings resolved

| Finding | Resolution | Xana commit |
|---|---|---|
| Desktop exposed controls whose runtime ownership belongs to later remote work. | Removed the speculative controller catalog/sidebar controls; current controls now describe only implemented behavior. | `e1e4528` |
| Unknown semantic codes could lose their inspectable identity and compact controls lacked accessible names. | Centralized label projection, retained unknown codes, and added explicit compact labels. | `e1e4528` |
| Desktop always attempted an embedded owner even when a compatible foreground host already existed. | Added authenticated attach-before-own composition, unclaimed-controller acquisition, observer fallback, and real loopback controller tests. | `3bbc131` |
| Static image admission bounded compressed bytes but not aggregate decoded expansion. | Added a checked 32 MiB estimated decoded-RGBA window in addition to count, source-byte, pixel, and edge policy. | `183c724` |
| A paused Desktop receiver could cause critical-update publication to time out and interrupt active runtime work. | Added a bounded deferred critical queue with priority receipts/stops and explicit snapshot resync after overflow; active work no longer waits for presentation. | `90abda5` |
| Settings, account, model, maintenance, and native-path control work could execute on GPUI's foreground executor. | Centralized Settings manager loading on the background executor and made mutations return refreshed typed snapshots before foreground entity updates. | `5bc5690` |
| EOF-delimited single-instance messages depended on platform-specific socket-close behavior and could reject a valid concurrent launch on Windows. | Replaced EOF delimiting with a versioned, checked 4-byte length frame under the existing 4 KiB ceiling; incomplete, oversized, and hostile JSON still fail closed. | `f4ba3f1` |

The large `Workbench` coordinator was not split merely to reduce line count.
It retains one place for coupled runtime-update, retained-entity, command, and
layout transitions, while focused components and domain authorities already
live in separate modules. A future extraction should require a demonstrated
ownership or independent test boundary rather than cosmetic file-size churn.

## Scope and authority retained

- GPUI, `gpui-component`, and `gpui-ai` remain presentation dependencies of the
  separately buildable Desktop package; the default CLI/TUI graph does not
  resolve them.
- Desktop receives bounded snapshots and emits typed intent. Provider, tool,
  credential, filesystem, artifact, policy, and process authority remains in
  the Xana runtime or its narrow Desktop control adapters.
- Attached Desktop never performs implicit takeover and never stops an
  external foreground owner when it closes.
- Local web, remote control, public protocol stability, arbitrary extension UI,
  native Lottie, broad media playback, M6 paging/memory/retrieval, M7 evaluation,
  and M8 remote scope remain outside M4.
- Proposal 0022 and ADR 0003 remain **Accepted**, not **Implemented**, until the
  owner and supported-platform evidence below is recorded.

## Automated verification

The deterministic final gate is:

```console
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-targets --no-default-features
cargo doc --locked --workspace --all-features --no-deps
pwsh ./scripts/check-desktop-dependencies.ps1
pwsh ./scripts/check-package-contents.ps1
cargo package --locked --allow-dirty
pwsh ./scripts/measure-m4-interface.ps1
```

Focused regression coverage includes a paused presentation while an embedded
Run completes, attached Desktop routing through a real loopback host, incumbent
controller retention, 10,000-message snapshot bounds, 10,000 progressive
deltas, decoded-preview expansion, cache eviction/revisit, four concurrent mock
streams, and strict dependency/authority checks.

## Evidence still required

The owner manual record must still cover visual judgment, complete keyboard and
focus paths, selection/copy, IME, 200% reflow, reduced motion, screen readers,
60 Hz and available native 120+ Hz behavior, sustained scrolling/streaming,
image-cache return, close/reconnect, notifications, and Desktop/TUI/plain
semantic feel. Windows x64, Linux x64 glibc, macOS ARM64, and macOS Intel must
each supply the required source-build/lifecycle evidence. Warm/cold readiness,
input-to-paint, runtime-event-to-paint, settled redraw, CPU, RSS/private memory,
GPU/backend, scale, power mode, and display details must be measured rather
than inferred from data-plane timings.

Until those observations are recorded, M4-21 through M4-24 and the aggregate
milestone remain in progress even when all deterministic checks pass.
