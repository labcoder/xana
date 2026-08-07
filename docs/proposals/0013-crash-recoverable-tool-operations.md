# Crash-recoverable tool operations

> Audience: Contributors and coding agents
> Authority: None
> Status: Implemented

## Context

Durable sessions previously exposed an operation that was running at process
exit without distinguishing a tool that never became eligible from one whose
effect happened before its result record. This historical proposal accepted
the implemented write-ahead invocation and explicit recovery boundary without
accepting automatic replay, parallel effect batches, distributed transactions,
or a durable compute heap from Proposals 0003 and 0004.

## Accepted records and identities

Version 1 adds `ToolResultId` and `NamedValueId`, plus distinct records for:

- operation acceptance, binding the thread and committed user entry;
- step start, binding one assistant response and its ordered tool-call batch;
- invocation intent;
- invocation result;
- operation suspension and terminal outcome; and
- named durable values.

An invocation intent binds operation, step, invocation, preallocated result,
provider call id, tool name, tool contract version, normalized final arguments,
the complete current permission audit fact, canonical scope through that fact,
and the tool's saved replay declaration. A result must match exactly one intent
and preallocated id and is completed, failed, declined, or interrupted.
Completed output is a bounded inline JSON value or immutable artifact reference.
Named values may also reference a committed context id/version.

The reducer preserves append and call order and rejects duplicate or mismatched
identities, intents outside a started step, second results, illegal operation
transitions, dangling values, and a terminal operation with an unclassified
pending invocation.

## Accepted execution order

Steps execute calls serially. For each call Xana plans immutable executable
data, authorizes its final arguments and scope, commits the permission audit,
preallocates a result id, commits and flushes intent, and only then executes.
After the effect, Xana stores bounded output, commits and flushes the result,
then commits the correlated model-visible tool result and head move. Events for
these facts follow their commits; live text deltas remain transient.

A planning or intent-append failure performs no effect. A result-append failure
after execution is a durability failure and leaves intent without result; heap
state and live events cannot fill that gap. This remains process-crash recovery
at flushed record boundaries, not a power-loss or `fsync` guarantee.

Built-in tool contract version starts at `1`. It changes when argument meaning,
effect behavior, or replay semantics changes. `read_file` and `list_files`
declare `Safe`; `edit_file` and `run_command` declare `Never`. Effect class,
name, command text, and a previous approval never imply replay safety.

## Recovery and partial batches

Read-only recovery planning walks original step/call order:

- a matching result is already complete and is never repeated;
- no intent is not eligible to execute during recovery;
- an intent without result is replayable only when saved and current
  declarations are both `Safe` and the same name/contract version exists;
- every other unknown outcome becomes an `Interrupted` result with the
  preallocated id and is never executed; and
- earlier completed calls in a partial batch remain complete while the first
  missing result is reconciled before later calls.

Opening, inspecting, and `xana --resume SESSION_ID` only restore and report.
`xana operation plan --session SESSION_ID OPERATION_ID` renders identifiers and
actions without arguments. Only explicit `xana operation resume --session
SESSION_ID OPERATION_ID` or the equivalent typed `ResumeOperation` runtime
command may reconcile.

Before any safe replay, Xana replans the exact persisted arguments, verifies
the resulting normalized arguments and canonical scope, re-evaluates current
permission, commits the new audit/recovery decision, and then repeats once. A
current denial records `Declined` without execution. Missing/incompatible tools
record `Interrupted`. Completed results are never duplicated. One foreground
writer handles a resume; concurrent ownership is unsupported and rejected.

Recovery in this increment reconciles the original ordered batch and finishes
the interrupted operation. It does not call the conversational provider to
invent continuation. Unknown `Never` outcomes produce a bounded correlated
tool-result conversation entry explaining that manual reconciliation may be
required.

## Context and state limits

Named intermediate values refer only to bounded inline JSON, immutable
artifacts, or committed context versions. Pure selection over an immutable
artifact may be recomputed, but live project snapshotting remains a separately
recorded effect when it occurs inside an operation. No process, interpreter,
socket, open file, or heap snapshot is authoritative recovery state.

## Deliberate exclusions

This proposal excludes automatic retries, background recovery, parallel effect
batches, generalized idempotency, session portability, multi-process writers,
distributed transactions, power-loss durability, and containment. Replayed
tools still use Xana's ordinary host permissions.
