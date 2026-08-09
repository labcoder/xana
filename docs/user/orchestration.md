# Child orchestration

> Audience: People configuring or observing Xana child agents.

Xana can currently delegate one bounded task from a native root conversation
to an exact native task route. This is runtime-owned work, not a second shell
process and not a free-form background agent.

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

When at least one route exists, the native root tool catalog includes
`delegate_agent`. You can ask naturally, for example:

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
permission ceiling.

## Context, identity, and reports

Each child starts a fresh native conversation containing Xana's built-in
identity and guidance, the explicit task, the actual child tool catalog,
environment facts, and a bounded root `AGENTS.md` view when present. It does
not receive the parent's full conversation. A profile-selected child tool
registry never contains `delegate_agent`, so children cannot create children.

`spawn_agent` creates a durable handle keyed by `AgentId`; `await_agent` reads
its terminal report. The model-facing `delegate_agent` convenience performs
both inside one tool call. If its caller stops awaiting, the runtime still owns
the child. Completed and failed reports retain parent/child, operation, thread,
route, connection, model, and owner attribution. Inline output is bounded by
the route's `max_report_bytes`. Provider usage is reported as `unknown` until
an adapter supplies a real measurement; Xana does not substitute zero.

## Current limits

- One native child may be active at a time.
- There is no detach or background continuation.
- Parallel batches, explicit cancellation/timeouts, offline child inspection,
  artifact-backed report overflow, orchestration plans, and managed Codex
  children are not implemented in this slice.
- Runtime shutdown owns the child task; process restart never implies that a
  local child is still running.
- `xana route` commands diagnose routes but do not start children.
