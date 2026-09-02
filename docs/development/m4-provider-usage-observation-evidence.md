# M4 provider and account usage observation evidence

> Audience: Contributors and coding agents
> Authority: Descriptive

This record closes the implementation evidence for M4-04B. It does not claim
durable analytics, billing, forecasting, or continuous telemetry.

## Implemented boundary

- Native OpenAI-compatible and Anthropic adapters preserve provider-reported
  cache, reasoning, token, cost, and request-affinity facts without exposing a
  raw request ID. Xana measures serialized prompt and tool-schema bytes
  independently.
- Managed Codex usage preserves cumulative input, cached input, output,
  reasoning, total, last-input context occupancy, and context capacity when
  app-server reports them.
- Stable usage observations distinguish request/run/conversation/connection/
  model/account/rate-limit scope, delta versus cumulative accounting, period,
  source, authority, freshness, availability, and reset.
- The ledger deduplicates replays, replaces newer cumulative snapshots, rejects
  out-of-order inflation, and isolates reset periods.
- `xana usage` returns one provider-neutral text or JSON report. It can request
  an explicit refresh, but rendering and startup never poll.

## Account authority

- Codex reads rate-limit windows only through the vendor-owned app-server and
  only for its ChatGPT account mode.
- OpenRouter reads the configured key's `/key` facts. It reads `/credits` only
  after the provider says that credential is a management key.
- OpenAI and Anthropic organization endpoints are not contacted with ordinary
  inference credentials. Their account observations report that separate
  management authority is required.
- Ollama and generic compatible endpoints report account inspection as
  unsupported.

No test or report contains raw credentials, authorization headers, provider
response bodies, account identifiers, or raw request IDs.

## Resource and failure bounds

| Boundary | Value |
|---|---:|
| Source response and cache document | 256 KiB |
| Observations per refresh | 128 |
| Fresh cache age | 60 seconds |
| Minimum forced-refresh interval | 1 second |
| Source timeout | 15 seconds |
| Retry | one, after 150 ms, only for retryable failure |
| Retained request-affinity inputs per native turn | 64 digests |

Failed refresh uses a marked-stale cache when valid. Cancellation returns a
cancelled operation rather than inventing an account observation. Corrupt or
oversized caches are ignored and replaced only by validated bounded data.

## Verification

The implementation passed locally on Windows with Rust 1.97.1:

- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --workspace --all-targets --all-features`: 965 passed, 6 ignored
  library tests; 20 CLI tests; 4 settings CLI tests; 3 Desktop tests; and
- `cargo test --workspace --all-targets --no-default-features`: the same suite
  and result.

Focused fixtures cover OpenRouter exact/management/permission/429/malformed/
oversized responses, Codex primary/reset and malformed limits, unsupported and
management-gated providers, provider token categories, stable IDs, delta and
cumulative replay, reset isolation, fresh/rate-limited/stale/corrupt cache,
bounded retry, cancellation, and absence of idle polling.

Cross-platform CI remains the repository gate at the next authorized push.
