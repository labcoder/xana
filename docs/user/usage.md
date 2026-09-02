# Usage, limits, and model facts

> Audience: People using Xana

Xana keeps current-process counters separate from provider account facts. A
missing number means unknown or unsupported; it never means zero, unlimited,
or free.

## Inspect usage

Run the provider-neutral usage command for the active connection and model:

```console
xana usage
xana usage --json
```

The first call may contact an account source. Later calls reuse a bounded cache
for up to 60 seconds. Request a live refresh explicitly:

```console
xana usage --refresh
xana usage --connection openrouter --model openai/gpt-5 --refresh
```

An explicit refresh within one second of the cached observation is rate-limited
and returns that cache. A retryable upstream failure receives one retry. If a
later refresh fails, Xana returns prior facts marked `stale` when it can; with
no usable cache it reports an exact unavailable reason. The command is
cancellable and never starts an idle background poller.

`--model` reports capability, context-window, output-limit, and pricing facts
from the selected connection's configured or cached model catalog. It requires
`--connection`. Account refresh does not refresh that catalog; use
`xana model refresh CONNECTION` for model discovery.

Inside an interactive conversation, `/usage` (or `/usage compact`) adds a
compact scoped card and `/usage details` opens the complete scrollable semantic
report. The report can contain current-Run, current-Conversation, and
current-process accounting; context capacity; prompt-plan categories;
execution facts; completion receipts; and provider-account facts that were
already observed. It is not an account-balance refresh. Missing facts remain
explicitly unavailable, and process-local counters are never labeled as
durable Conversation totals.

Plain mode supports the same `/usage compact` and `/usage details` forms. The
detailed append-only report prints the bounded execution facts, prompt plan,
and completion receipts already retained by that client. Private
`--output stream-json` automation emits those same current-Run facts once in a
`summary` frame before its final result instead of requiring a consumer to
reconstruct them from incremental observations.

## What Xana can report

| Source | Current account facts | Credential behavior |
|---|---|---|
| Managed Codex with ChatGPT login | Primary/secondary rate-limit percentage, window, and reset when app-server reports them | Codex owns login and credentials |
| OpenRouter | Configured key usage and limit; purchased/used/remaining credits for a management key | The inference key is used for `/key`; `/credits` is called only after OpenRouter identifies it as a management key |
| OpenAI API | Management facts are permission-gated | Xana never broadens an ordinary inference key into organization-admin authority |
| Anthropic API | Management facts are permission-gated | Xana never broadens an ordinary inference key into organization-admin authority |
| Ollama or custom OpenAI-compatible endpoint | Account facts are unsupported | No account request is attempted |

Codex API-key mode does not expose ChatGPT subscription limits through this
path. Provider endpoints may omit any numeric field, and provider support can
change independently of Xana.

## Accounting meanings

- Native provider responses are per-request deltas. Xana can retain reported
  input, cached-input, cache-write, output, reasoning, tool, total-token, and
  cost values.
- Managed Codex token updates are cumulative snapshots. Newer sequence values
  replace older values for the same period instead of being added again.
- Prompt and tool-schema byte counts are local serialized-resource facts. They
  are not token estimates or provider billing truth.
- Context occupancy, account quota, credits, rate limits, and cost remain
  separate values with their own source and availability.
- A reset starts a new accounting period. Replayed observations are
  deduplicated by stable identity.

Raw provider responses, authorization headers, credentials, account IDs, and
provider request IDs are not written to the report or cache. When available,
request affinity is represented only by a bounded digest.

## Cache and troubleshooting

Non-secret account observations are cached per connection beneath
`XANA_HOME/cache/usage/`. Each document and response is bounded to 256 KiB and
each refresh to 128 observations. Source work has a 15-second bound.

Use `xana usage --json` to distinguish:

- `available`: a source reported the fact;
- `stale`: a previous fact survived a failed refresh;
- `permission_required`: the configured authority is insufficient or missing;
- `unsupported`: the connection has no supported account source; and
- `unavailable`: a supported source could not provide a valid bounded result.

Usage inspection failing does not prevent chat unless the provider itself
rejects generation because of an execution-blocking limit.
