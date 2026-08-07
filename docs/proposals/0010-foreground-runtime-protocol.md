# Foreground runtime protocol

> Audience: Contributors and coding agents  
> Authority: Prescriptive  
> Status: Accepted

## Context

Xana's blocking terminal currently owns conversation history and calls the
headless agent directly. Streaming, approvals, persistence, and later
frontends need a logical application owner and a typed boundary before any
physical crate split is justified. Proposals 0001 and 0005 contain broader
runtime, frontend, thread, and concurrency designs; this proposal accepts only
the foreground slice needed now.

## Accepted component boundary

One foreground runtime module inside the existing binary crate owns transient
conversation history and at most one active root operation. A terminal is a
client: it sends typed control commands and renders passive events. The
headless `Agent` receives owned prompt, provider, tool, and operation services;
it does not read terminal input or mutate frontend state.

The implementation becomes asynchronous end to end through the provider and
tool turn path. The executable uses a Tokio multi-thread runtime. The logical
boundary is exercised before the proposed `xana-core`, `xana-runtime`, and
`xana-cli` workspace split; this decision neither accepts nor implements that
split.

## Accepted protocol

Control crosses a bounded Tokio channel as serializable `RuntimeCommand`
values. The initial commands are:

- submit one root turn with a preallocated operation id and user input;
- clear transient conversation while no turn is active;
- decide one correlated provisional command approval; and
- shut down the foreground runtime.

Observation crosses a single-client unbounded channel as serializable
`AgentEvent` values. Events report operation state, live assistant text deltas,
provisional approval requests, tool completion, final assistant messages,
conversation clearing, and rejected commands. An event is passive: a closed,
slow, or failed receiver cannot authorize, cancel, retry, or otherwise change
execution. The single foreground channel is not a replay log or multi-client
broadcast contract.

`OperationId`, `StepId`, and `ToolInvocationId` are distinct UUID-backed
newtypes serialized transparently as strings. Operations are explicitly
running, suspended, or finished. Finished state always carries a completed,
failed, declined, or interrupted outcome. Live text deltas are disposable;
the accumulated final internal message is the completed value carried forward.

## Streaming and provider boundary

OpenAI-compatible requests use streaming transport. An incremental bounded SSE
decoder treats arbitrary network chunks as bytes rather than frames, supports
LF and CRLF framing, comments, multi-line data, and `[DONE]`, and rejects
oversized or incomplete frames. A private accumulator joins text and indexed
tool-call fragments, validates completed ids, names, and JSON arguments, and
produces the existing provider-neutral assistant message.

A private object-safe asynchronous chat transport seam enables scripted tests.
It is not the final product provider trait. Provider wire types remain private
to the adapter.

## Provisional approval transport

Until the permission protocol replaces it, `run_command` approval uses the
same command/event plane. A coordinator registers one pending oneshot by
operation and invocation id, emits the exact requested action, suspends the
operation, awaits only the matching decision, removes pending state on every
exit, and returns to running before an approved effect. Unknown, stale,
duplicate, or mismatched decisions are rejected. Losing the controlling client
fails closed.

This is correlation and transport, not permission policy, a durable audit
record, or process containment.

## Rejected expansion

This accepted proposal does not add or authorize:

- the physical Rust workspace split;
- a daemon, server, or background operation;
- multi-client attachment, snapshots, replay, or broadcast buffering;
- durable sessions, threads, or operation recovery;
- child agents, hooks, telemetry, or structured concurrency;
- general cancellation guarantees across provider or process boundaries; or
- a final provider plugin interface.

Those designs remain non-authoritative in Proposals 0001 and 0005 until
separately accepted.
