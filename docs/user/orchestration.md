# Child orchestration

> Audience: People configuring or observing Xana child agents.

Xana can delegate one bounded task or atomically admit a fixed batch from a
native root conversation to exact task routes. Each route can select a native
Ollama, OpenAI-compatible, OpenAI API, OpenRouter, or Anthropic model, or a
managed Codex connection. Xana owns admission, supervision, attribution, and
the terminal report. A native child runs in process; a Codex child uses a fresh
vendor-owned app-server process and thread. Neither is detached or a free-form
background agent.

## Configure and verify a route

Configuration version 3 separates reusable profiles from stable task routes:

Full Custom Setup or `xana setup --section profiles-routes` can create or edit
these exact bindings and their bounded orchestration limits through the same
production validator. Sectional edits preserve unrelated TOML and comments.
They affect a new conversation only; setup never changes an already resolved
child route or grants capabilities absent from the root snapshot.

```toml
version = 3
default_profile = "default"
default_child_route = "worker"
permission_mode = "ask"

[providers.local]
kind = "ollama"
base_url = "http://localhost:11434/v1"

[providers.local.models."qwen3:1.7b"]
tools = true

[profiles.default]
connection = "local"
model = "qwen3:1.7b"

[profiles.default.orchestration]
max_fan_out = 4
max_descendants = 8
max_concurrency = 2
deadline_seconds = 300
max_context_tokens = 8192
max_report_bytes = 32768
max_artifact_bytes = 8388608

[profiles.worker]
connection = "local"
model = "qwen3:1.7b"
capabilities = ["fs.read", "fs.list", "xana.docs.read"]
permission_mode = "ask"
max_tool_rounds = 4

[profiles.worker.orchestration]
max_context_tokens = 8192
max_report_bytes = 32768
deadline_seconds = 300

[routes.worker]
profile = "worker"
```

Check the exact local snapshot before chat:

```text
xana route list
xana route check worker
```

These diagnostics never log in, refresh a catalog, contact a model, or start a
managed process. An unavailable route fails rather than choosing a fallback.

A managed route references a configured Codex connection and a model already
present in Xana's refreshed local Codex catalog. Because Codex owns its inner
tools, the profile must request no Xana-native capabilities:

```toml
[profiles.codex-review]
connection = "codex"
model = "ADVERTISED_MODEL_ID"
capabilities = []
permission_mode = "ask"

[routes.codex-review]
profile = "codex-review"
```

Before delegating, run `xana connection login codex`, `xana model refresh
codex`, and `xana route check codex-review`. A child uses the configured Codex
CLI app-server, not a running Codex desktop process. Managed child routes must
resolve to `ask` or `allow`; an effective `deny` route fails closed because the
current app-server contract cannot guarantee that all Codex-owned inner tools
are disabled.

## Delegate during native chat

When at least one route exists, the native root tool catalog includes the
explicit `spawn_agent`, `spawn_many`, `await_agent`, `collect_agents`, and
`cancel_agent` operations plus `delegate_agent`. You can ask naturally, for example:

```text
Please delegate a focused review of the configuration parser to the worker
route, then use its findings in your answer.
```

The model supplies an explicit task and optional route. Omitting the route uses
only `default_child_route`; Xana never guesses. Xana displays typed child
events similar to:

```text
xana> child <id> [worker via local/qwen3:1.7b]: Admitted
xana> child <id> [worker via local/qwen3:1.7b]: Queued
xana> child <id> [worker via local/qwen3:1.7b]: Running
xana> child <id> report: Completed
```

Permission questions identify the child and route. The root remains the
controlling terminal, and the child profile can only narrow the configured
permission ceiling. The root profile is the parent authority and budget
ceiling; a child profile and request may narrow it but cannot widen it.

`delegate_agent` is the efficient one-task path: it spawns and waits inside one
tool call. `spawn_agent` returns immediately with a handle, allowing the root
turn to finish while Xana continues supervising the child. A later root turn
can call `await_agent` or `cancel_agent`; this also makes the active slash
commands useful between turns. `await_agent` accepts an optional bounded
`timeout_ms` and an explicit `cancel_on_timeout` flag. `cancel_agent` returns a
request receipt, not a fictional terminal success.

`spawn_many` takes a statically bounded array of independent child requests.
Xana validates and reserves the complete batch before one handle or event is
visible. A bad member or aggregate limit rejects the entire batch. Accepted
children queue in input order and run fairly up to `max_concurrency`.

Each request can ask for a plain `summary` (the default) or a JSON result.
JSON must parse successfully and is stored in canonical form; an unknown schema
is rejected before admission. `collect_agents` takes 1–64 unique child handles,
returns entries in that same order, and supports a shared `timeout_ms` plus
`continue_on_error` (default) or `fail_fast`. Stopping a fail-fast wait does not
discard earlier reports. Remaining work is cancelled only when
`cancel_remaining` is explicit; collection timeout similarly cancels only with
`cancel_on_timeout`.

## Context, identity, and reports

A native child starts a fresh conversation containing Xana's built-in identity
and guidance, the explicit task, the actual child tool catalog, environment
facts, and a bounded root `AGENTS.md` view when present. A managed child starts
a fresh ephemeral Codex thread with Xana's identity developer instruction, the
exact route model/options and workspace policy, and the explicit task. Codex
continues to own project-instruction discovery, its inner context, tools,
sandbox, and inference. Neither owner receives the parent's full conversation.
Native child registries omit orchestration tools, and managed child routes
claim no Xana capabilities, so children cannot create Xana children.

The root may explicitly hand off up to eight selected text previews (16 KiB
each, 64 KiB in aggregate) and sixteen immutable artifact references. Text is
marked as untrusted parent-selected context and competes for the child's
configured prompt budget. Artifact handoff includes only logical id and
content hash metadata; it never copies an artifact body into the prompt. For
example:

```json
{
  "route": "reviewer",
  "task": "Review the selected parser branch.",
  "handoff": {
    "previews": [
      {"label": "parser branch", "content": "selected bounded source text"}
    ],
    "artifacts": []
  }
}
```

`spawn_agent` creates a durable handle keyed by `AgentId`; `await_agent` reads
its terminal report. The model-facing `delegate_agent` convenience performs
both inside one tool call. If its caller stops awaiting or an await times out,
the runtime still owns the child. Timeout does not cancel work unless the
caller explicitly selects cancel-on-timeout.

During an active foreground process, use:

```text
/agents
/agent AGENT_ID
/cancel-agent AGENT_ID
```

The list and detail views show lineage, route, execution owner, connection,
model, lifecycle, usage state, and report reference. They do not copy a
completed inline report body into the control event; use `await_agent` or
`collect_agents` when the report itself is needed. Cancellation first prints
that a request was made; only the later `Cancelled` lifecycle/report proves the
child stopped. Repeating cancellation or awaiting a terminal child is safe and
returns the existing state.

Another Xana process cannot control this foreground runtime. It can only read
durable facts:

```text
cargo run -- session inspect SESSION_ID
```

If Xana restarts after an admitted, queued, running, or suspended child record,
inspection projects that child as `Interrupted` without appending a record,
calling a provider/tool, or replaying work. The output labels this as a
read-only restart projection.

Completed, failed, cancelled, and interrupted reports retain parent/child,
operation, thread, route, connection, model, and owner attribution. Inline
output is bounded by the route's `max_report_bytes`. Provider usage is
explicitly `measured`, `estimated`, or `unknown`; Xana does not substitute
zero. Native children always measure provider request count. OpenAI and
OpenRouter request streaming usage, Anthropic maps its input/output fields,
and compatible endpoints may emit optional usage. A field absent from any
request remains unknown for the complete child turn. A Codex child maps the
app-server's thread/turn token-usage notification when available. Its request
count is one managed turn, not an estimate of private upstream calls; missing
token fields and spend remain unknown. A
hard token or spend request is rejected unless its execution owner exposes enforceable
pre-request control or an interruptible live meter. Subscription rate-limit
state is not monetary spend.

Completed output one byte beyond the inline limit is stored as an immutable
artifact when it fits `max_artifact_bytes`. The report contains a bounded
preview, content-addressed reference, and exact byte length. Collection does
not load artifact bodies into the model-facing result; it verifies the stored
length and digest and reports missing or corrupt bytes on that child entry.
The complete collection JSON is independently capped at 256 KiB, with each
model-facing preview capped at 2 KiB. Repeating an await or collection after
terminal state returns the same durable report evidence.

## Current limits

- Children run up to the root profile's `max_concurrency`; additional
  admitted children wait in stable admission order.
- The runtime atomically accounts for fan-out, total admitted descendants,
  tool rounds, context bounds, inline report bytes, and artifact bytes. These
  are session-wide ceilings and are not replenished by sequential completions;
  only the concurrency slot is reusable. A child's wall-clock deadline begins
  at admission, including queue time.
- There is no detach or background continuation.
- Mixed-model children keep independent histories; Xana does not translate,
  merge, or summarize them through an additional model call.
- Every managed Codex admission starts a new ephemeral app-server thread; Xana
  does not resume a foreground managed handle or reattach a child after process
  loss. Closed owner-neutral plans are described in
  [Orchestration plans](orchestration-plans.md).
- Managed cancellation sends one correlated `turn/interrupt` request and keeps
  consuming the terminal race until one absolute three-second deadline from
  the observed cancellation. Cancellation also
  races startup/account/thread setup and prevents turn start once observed. An
  older/incompatible app-server that rejects interruption is closed and
  reported as a failed child with the typed remote error; Xana does not claim
  that cancellation succeeded.
- Each supervised managed approval re-enters the child broker. A matching Xana
  session grant can avoid a repeated user prompt, but Xana still sends Codex
  only one-effect `accept`; it never delegates session approval scope to the
  app-server and declines when one-effect acceptance is unavailable.
- Child live activity uses a 256-event producer queue and is projected up to
  4,096 non-control events or 4 MiB per child. Xana then drops further
  non-control activity and emits one warning. Permission requests use a
  separate control lane, so truncation cannot hide an approval that must be
  decided or denied.
- Runtime shutdown cooperatively cancels queued and running children, closes
  pending permission requests, waits for their durable terminal outcomes, and
  uses a bounded interrupted fallback only for an unresponsive execution.
- Process restart never implies that a local child is still running.
- `xana route` commands diagnose routes but do not start children.

## Optional native account smoke

This check is owner-run and is not required in CI. Configure two exact native
routes backed by credentials already stored through Xana's documented
credential references. Run `xana route check` for each route, then ask the root
to `spawn_many` one short task per route and collect both reports. Confirm that
the displayed route, connection, model, request count, and any provider-exposed
token fields remain distinct. When recording evidence, retain only those
non-secret labels and terminal statuses; redact prompts, report bodies, HTTP
headers, account identifiers, and credential-source values.

## Optional managed Codex smoke

This owner-run check is also outside CI. After login and catalog refresh,
configure a `codex-review` route like the example above and confirm `xana route
check codex-review`. From a native root, delegate a harmless task such as
summarizing one source file without changes. Confirm that the terminal shows a
fresh managed thread, attributed reasoning/plan/tool activity, the exact route,
connection and model, and a bounded terminal report. Start a second task and
cancel it with `/cancel-agent AGENT_ID`; confirm the later terminal status is
`Cancelled`. If the child requests an effect, verify the prompt names the child
and that denial affects only that child. Record only non-secret identifiers,
versions, statuses, and usage totals; redact prompt/report content, account
identifiers, and protocol headers.
