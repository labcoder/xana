# Child orchestration

> Audience: People configuring or observing Xana child agents.

Xana can delegate one bounded task or atomically admit a fixed batch from a
native root conversation to exact native task routes. Each route can select a
different Ollama, OpenAI-compatible, OpenAI API, OpenRouter, or Anthropic
connection and model. This is runtime-owned work, not a second shell process
and not a free-form background agent.

## Configure and verify a route

Configuration version 3 separates reusable profiles from stable task routes:

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

Each child starts a fresh native conversation containing Xana's built-in
identity and guidance, the explicit task, the actual child tool catalog,
environment facts, and a bounded root `AGENTS.md` view when present. It does
not receive the parent's full conversation. A profile-selected child tool
registry never contains `delegate_agent`, so children cannot create children.

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
model, lifecycle, usage state, and report reference. Cancellation first prints
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
request remains unknown for the complete child turn; spend remains unknown. A
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

- Native children run up to the root profile's `max_concurrency`; additional
  admitted children wait in stable admission order.
- The runtime atomically accounts for fan-out, active descendants, tool
  rounds, context bounds, inline report bytes, and artifact bytes. A child's
  wall-clock deadline begins at admission, including queue time.
- There is no detach or background continuation.
- Mixed-model children keep independent histories; Xana does not translate,
  merge, or summarize them through an additional model call.
- Managed Codex children are not implemented in this slice. Closed native
  plans are described in [Orchestration plans](orchestration-plans.md).
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
