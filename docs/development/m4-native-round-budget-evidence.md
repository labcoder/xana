# M4 native round-budget evidence

> Audience: Contributors and coding agents
> Authority: Descriptive

This record validates the M4-03D native round-budget lifecycle. The feature
turns the profile's finite `max_tool_rounds` value into an explicit soft
tranche while preserving Xana's immutable 256-round ceiling and all previously
committed work.

## Contract exercised

The deterministic fixtures prove:

- below-budget work completes normally, while exhaustion commits a typed
  suspension before any surface can offer a decision;
- Continue addresses the exact operation and suspension, retains one user Turn
  and one operation id, preserves ordered tool requests/results and history,
  accumulates provider usage and round counts, and grants only the next
  configured tranche;
- multiple continuations cannot exceed the 256-round root ceiling, where Stop
  becomes the only allowed action;
- Stop is a durable atomic terminal `Declined` decision that never rolls back a
  committed effect;
- restart re-emits the same unresolved suspension identity, while stale,
  duplicate, mismatched, and disallowed decisions fail closed;
- a crash immediately after a committed Continue is restored as explicit
  unfinished running work and the normal recovery plan terminates it without
  replaying a provider or tool call;
- repeated exact tool target/argument patterns are counted as diagnostic facts
  without silently terminating or automatically continuing the operation; and
- frontend protocol v3, plain mode, the TUI, stream JSON, one-shot result v2,
  and Desktop preserve the same operation/suspension correlation. One-shot
  uses status/category `incomplete` and exit code 7 rather than waiting for an
  unavailable controller.

The foreground workspace lease remains owned across the suspension. Live plain
mode prompts immediately; restored plain mode consumes the re-emitted durable
suspension before accepting another prompt. TUI `/continue` and `/stop` and
Desktop Activity controls carry the exact opaque identities. Disconnect,
interruption, permission, root-gate, and frontend-pressure behavior continue to
use their existing bounded control paths; no suspension display text grants
execution authority.

## Budget and durability observations

| Fact | Bound or behavior |
|---|---|
| Configured soft tranche | `1..=64` rounds; default 8 |
| Immutable native root ceiling | 256 cumulative rounds |
| Continuation grant | `min(soft tranche, remaining root rounds)` |
| Provider usage | Cumulative; missing token fields remain unknown rather than becoming zero |
| Durable identity | One operation id plus a new exact suspension id at each boundary |
| Committed evidence | Cumulative step, invocation, result, round, usage, continuation, and repeated-pattern facts |
| Noninteractive outcome | Version-2 `incomplete`, exit 7 |
| Frontend command/event vocabulary | Version 3 |

Continue does not reset context admission, provider usage, child reservations,
permission decisions, deadlines, cost facts, or external-effect evidence. It
only admits another tool-round tranche under the fixed root ceiling.

## Verification

The required local gate passed on Windows x86-64 on 2026-09-01 with the
repository-pinned toolchain and lockfile:

```text
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features --no-fail-fast
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

The complete run passed 896 library tests with 6 intentionally ignored manual
or stress probes, 20 CLI integration tests, 4 settings integration tests, and 3
Desktop tests. Focused coverage includes the agent tranche, durable multi-
tranche continuation, restart/stop, hard-ceiling arithmetic, crash-after-
decision recovery, stale/duplicate decisions, frontend correlation, TUI
commands, Desktop projection, and one-shot incomplete result.

Windows passes locally. Linux and macOS compilation and tests await the next
authorized CI push and are not claimed here.
