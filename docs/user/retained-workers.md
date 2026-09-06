# Retained workers and bounded context operations

A retained worker keeps a completed child's identity, goal, selected evidence,
original authority ceiling, route and receipts after its process exits. A
follow-up is a fresh, explicitly started execution under the same parent. It
does not restore a model heap or make a child an independent background owner.

Protected storage is required. Commands also work as `/worker ...` from the
terminal/TUI; Desktop exposes them in the **Schedules → Retained workers**
section. In a source checkout, replace `xana` with `cargo run --locked --`.

## Retain, queue, inspect and run

First inspect the source Conversation and the completed child's report. Replace
the identifiers and expiry below with your actual values:

```text
xana worker retain --session SESSION_ID --agent CHILD_AGENT_ID --goal "Continue reviewing the selected evidence" --expires "2026-11-01T00:00:00-07:00" --evidence ARTIFACT_ID --authorize
xana worker list
xana worker inspect CHILD_AGENT_ID
xana worker follow-up CHILD_AGENT_ID --revision REVISION --request-id UNIQUE_UUID --text "Summarize the unresolved findings"
xana worker inspect CHILD_AGENT_ID
xana worker run CHILD_AGENT_ID --revision NEW_REVISION
```

Retain selects existing immutable artifacts; it does not crawl files or copy a
whole Conversation into a prompt. Queueing does not start a model. The request
UUID makes a repeated submission recognizable; reuse it only when retrying the
same text. Conflicting or stale revisions fail without silently overwriting a
message. Refresh/inspect after each mutation.

The mailbox holds at most eight pending messages, and a retained identity accepts
at most 64 distinct follow-up request IDs over its lifetime. Goals and follow-ups
are each limited to 8 KiB. Records are bounded to 128 KiB, with at most 1,000
retained identities in one protected home. Each continuation
receives its goal, the last bounded receipt, the new message, and metadata for up
to 16 most recently selected/derived evidence references—not the full history.
These are storage limits, not a promise that every combined handoff fits every
model's context budget. `run` checks the complete handoff before provider work
and rejects one that does not fit; queueing alone neither validates that model
budget nor spends provider tokens.

Execution rechecks the original scope, filesystem identity, route/configuration,
privacy generation, expiry, cancellation and cumulative parent allowance.
Changing a grant or forgetting source context can block continuation. Original
depth/fan-out and descendant budgets still apply; retaining or restarting does
not reset them. Both native and managed routes use the existing child supervisor.
Admission charges come from the indexed durable history, independently of which
completed child details remain in the compact execution snapshot.
For example, with the default eight-descendant allowance, the original child
leaves at most seven further executions under that parent (fewer if other
children already consumed it). The parent Conversation must not have another
active writer: a running chat and a retained continuation cannot independently
append to the same journal.

Every native tool execution rechecks the active execution identity and current
authority immediately before its effect. A previously prepared/approved call
does not bypass a stop, expiry, forgotten source or changed configuration.
Managed Codex runs a new bounded task: Xana does not claim to resume an unknown
vendor handle or synchronize subscription memory. Xana fences dispatch and
requests cancellation at its outer boundary; Codex owns checks and effects inside
its agent loop, which Xana cannot individually intercept.

```text
xana worker drain CHILD_AGENT_ID --revision REVISION
xana worker stop CHILD_AGENT_ID --revision REVISION
xana worker recover CHILD_AGENT_ID --revision REVISION --review-unknown
```

Drain refuses new messages; existing queued messages still require explicit
runs. Stop revokes continuation and requests cancellation. A lost/uncertain run
requires review and is not automatically replayed; recovery does not resubmit
its consumed message. Local cancellation cannot prove a provider stopped billing.
Desktop's **Interrupt this operation** cancels the current invocation, distinct
from the durable **Stop worker** action.

## Work with evidence without filling a prompt

`worker context CHILD_AGENT_ID --revision REVISION --operation JSON` supports
only these deterministic operations:

| Operation | Result |
| --- | --- |
| `search` | Matching evidence text with source ranges |
| `slice` | One selected byte range |
| `filter` | Selected text containing a literal string |
| `map` | Trim, lowercase or uppercase text |
| `reduce` | Count bytes, count lines or concatenate |
| `derive` | A labeled immutable result from selected ranges |
| `cite` | Source-cited selected evidence |

Each range names an exact artifact reference, byte offset and length; obtain
references from `worker inspect`, not a guessed path. The JSON discriminant is
`operation`, for example `{"operation":"slice","input":{"artifact":ARTIFACT_REFERENCE,"offset":0,"length":128}}`.
Use an appropriately quoted JSON argument for your shell. There is no Python,
shell, JavaScript or model-generated reducer to execute.

Limits are 16 input ranges, 64 KiB per range, 256 KiB total selected text and
result, and 16 MiB of full-artifact verification per operation. Verification
bytes count even when the returned slice is small. Workers retain at most 64
evidence references and share cumulative context limits of 64 MiB/256 operations
under their parent. Native retained execution can use the same `context_ops`
tool over explicitly selected evidence; managed execution has no invented tool
bridge. The native model must advertise tool support. In `ask` mode, retaining
selected evidence authorizes this exact built-in evidence scope; other requests
for additional authority are denied and require owner review. An explicit `deny`
permission mode remains a denial. These operations make zero model calls.

Only one context operation may be outstanding for a worker. Failed, interrupted
and unknown attempts retain their work charges. Owner recovery can mark unknown
work interrupted but does not refund its budget or replay the operation.

Immutable results retain provenance and citations. Invalid ranges, corrupt or
forgotten artifacts, changed scope and cancellation fail closed. Direct retrieval
is often simpler for a small answer: context operations are useful for bounded
selection and reuse, not a requirement for every read.

See [durable schedules](durable-schedules.md), [usage budgets](usage-budgets.md)
and [project evidence](project-recall.md).
