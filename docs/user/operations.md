# Operation recovery

> Audience: People inspecting or reconciling an interrupted Xana operation.

Xana records a tool invocation's exact authorized intent before its effect and
records the correlated result afterward. If the process stops between those
records, the effect's outcome is unknown. Missing local result data does not
mean the effect did not happen.

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
| `edit_file` | `Never` | record interruption; never repeat automatically |
| `run_command` | `Never` | record interruption; never repeat automatically |

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
