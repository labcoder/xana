# Operation recovery

> Audience: People inspecting or reconciling an interrupted Xana operation.

Xana records a tool invocation's exact authorized intent before its effect and
records the correlated result afterward. If the process stops between those
records, the effect's outcome is unknown. Missing local result data does not
mean the effect did not happen.

Installation diagnosis is a separate boundary. Use `xana doctor` (or
`/doctor` while idle) for redacted config, connection, Codex, path, terminal,
and stale-host findings; it never reconciles an interrupted tool effect.
Conversely, operation plan/resume never repairs installation state. See
[Configuration](configuration.md#diagnose-and-edit-configuration).

Restoring a session is always passive:

```text
xana --resume SESSION_ID
```

It performs no recovery effect. Inspect a specific unfinished operation with:

```text
xana operation plan --session SESSION_ID OPERATION_ID
```

The read-only plan prints session, thread, operation, step, invocation, and
result identifiers plus the proposed action. It does not print tool arguments.
The session id and unfinished operation ids are available from `xana session
inspect SESSION_ID` and normal terminal output.

## Ordinary round-budget suspension

A native operation that cleanly reaches its configured soft tool-round tranche
is not a crash-recovery case. Xana commits an exact round-budget suspension and
keeps the same operation, conversation history, tool results, usage, and
workspace ownership. Resume the session in the full-screen or plain interactive
surface and choose Continue or Stop (`/continue` or `/stop` in the TUI). The
same unresolved suspension identity is re-emitted after restart. One-shot
reports it as `incomplete` with exit code 7 rather than guessing a decision.

Continue admits one more configured tranche below the immutable root ceiling;
it does not reset token, time, cost, child, permission, or external-effect
accounting. Stop records a terminal declined outcome. Do not use `xana
operation resume` for this ordinary boundary: that command is reserved for the
unknown-effect recovery cases below.

## Explicit reconciliation

After reviewing the plan, reconcile exactly that operation with:

```text
xana operation resume --session SESSION_ID OPERATION_ID
```

Only an exact invocation whose saved and currently installed tool contracts
both declare `Safe` can run again. The tool name and contract version must
match, replanning must reproduce the saved final arguments and canonical
scope, and Xana reevaluates the current permission policy. An `ask` policy may
prompt again; the earlier approval is historical evidence, not ongoing
authority.

Current built-ins use this matrix:

| Tool | Replay declaration | Unknown outcome |
|---|---|---|
| `read_file` | `Safe` | eligible for one explicit, reauthorized replay |
| `list_files` | `Safe` | eligible for one explicit, reauthorized replay |
| `find_files` | `Safe` | eligible for one explicit, reauthorized replay |
| `grep_files` | `Safe` | eligible for one explicit, reauthorized replay |
| `write_file` | `Never` | record interruption; never repeat automatically |
| `edit_file` | `Never` | record interruption; never repeat automatically |
| `run_command` | `Never` | record interruption; never repeat automatically |
| `read_document` | `Safe` | eligible for one explicit, reauthorized replay |
| `xana_docs` | `Safe` | eligible for one explicit, reauthorized replay |

A missing tool, changed contract, changed scope, current `Never` declaration,
saved `Never` declaration, or current denial prevents execution. Xana records
a typed declined/interrupted result using the result id allocated before the
crash. Completed calls in an ordered multi-call step are preserved and are
never duplicated.

Recovery reconciles known records and terminates the interrupted operation. It
does not ask the model to continue the old turn. Start a new turn if follow-up
work is needed.

## Guarantees and limits

- Recovery is explicit; session open, inspection, and ordinary `--resume`
  perform zero tool effects.
- One process may own a session writer or recovery controller at a time.
- Complete flushed JSONL records are the authority. Live events, text deltas,
  channels, and process memory are not recovery state.
- The guarantee covers process crashes at record boundaries. It is not a
  power-loss, `fsync`, transactional-filesystem, or general idempotency
  guarantee.
- An unknown `Never` effect may already have happened and can require manual
  reconciliation outside Xana.
- Replayed tools still run with the Xana process's ordinary host permissions.
  Recovery is not containment or a sandbox.

## Foreground-host shutdown

`xana serve` remains the sole owner of work it starts. Ctrl+C stops new client
intake, fails pending controller approvals closed, interrupts the root, and
asks the native child supervisor or managed Codex driver to shut down. Normal
cleanup has a two-second target and exact owned-task escalation has a
five-second hard bound. Tool subprocesses and Codex app-server are configured
for kill-on-drop ownership; no cleanup path selects an unrelated process by
name or an unverified stale PID. A client disconnect alone does not stop
already-authorized work unless it held the controller lease, whose separate
three-second grace is documented in [Local foreground host](local-host.md).
