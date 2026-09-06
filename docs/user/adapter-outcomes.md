# Durable adapter command outcomes

Local adapters can correlate a native turn before sending it and later inspect
its durable outcome, even if the adapter disappears before receiving the final
event. This is a supported Rust facade in `xana::desktop`, not a remote API or a
second execution engine.

1. Obtain `client.command_outcomes(adapter_namespace_uuid)` for the selected
   native Conversation.
2. Call `prepare(input, attachments)` and durably save the returned
   `DesktopCommandKey` in the adapter before submission. Preparing a key does
   not admit or execute work.
3. Call `client.submit_correlated(key, input, attachments)`. The immediate
   receipt acknowledges only the bounded bridge queue, not durable completion.
4. Call `reader.lookup(&key)` on a background executor. A completed outcome
   contains a stable result entry id and content digest; `read_result(&key)`
   materializes that exact typed result without searching conversation prose.
5. After a process restart, construct `DesktopLaunch` with the explicit Xana
   home and workspace, then call its `command_outcomes(namespace, conversation)`
   method. This opens a read-only inspection capability, not a provider, session
   writer, migration, or resumed execution.

The namespace partitions correlation records. It is not a credential and does
not grant authority. Existing local same-user storage custody, immutable
Conversation Profile, selected Conversation, workspace and controller rules
still apply. A wrong scope, changed Profile, unrelated restored store, locked
or incompatible storage, or unavailable history fails closed. Cached readers
use their held revocable storage owner rather than silently unlocking a new
connection during lookup. Offline protected inspection uses the existing OS
custody or the explicitly configured recovery-key file once at construction;
credentials are never supplied in the correlation key or reacquired on lookup.
Backend termination and client shutdown directly revoke live readers, even
when the adapter has not drained its event queue. A new offline reader can then
inspect retained terminal evidence or report an unfinished admission as unknown.

## What an outcome means

| State | Meaning |
| --- | --- |
| Completed | The native terminal commit and exact assistant result are recorded. |
| Failed / Declined / Interrupted | The corresponding terminal outcome was committed. |
| Suspended | A retained budget decision, or a currently owned permission wait, blocks progress. |
| Pending | This reader's live owner still observes the admitted operation running. |
| Unknown | Admission exists, but neither a terminal commit nor a current live owner proves its outcome. |
| NotFound | This retained history has no admission for this key. |
| Unavailable | The history or required scope/custody cannot be safely inspected. |

A failed or interrupted operation may still have an exact committed assistant
result (for example, an answer committed before a completion check failed).
That reference remains inspectable; its presence does not change the terminal
state or prove that the task succeeded.

`NotFound`, `Unknown` and `Unavailable` are **not permission to retry**. An older
backup may predate admission, and an external effect may have happened before a
lost terminal commit. Lookup never replays work. Re-submitting an already
admitted key is rejected before another provider/tool execution; reusing the
key with different input is also rejected. Adapters should deduplicate delivery
using the stable command id and result reference. Xana does not promise
exactly-once external effects or exactly-once adapter delivery.

Native images use the same exact payload binding and existing attachment
limits. Managed vendor-owned turns explicitly reject correlated submission;
their opaque receipts are not presented as native durable evidence.

## Retention and cost

Correlation is stored in the existing operation admission and terminal records.
Protected history indexes survive execution-cache trimming and compaction. A
compatible protected backup retains those records and identities. There is no
new unbounded in-memory outcome list, background poller, or additional journal.
Exact operation and output-lineage inspection is bounded to 4,096 records and
the existing 16 MiB history read limit per bounded interval; individual records
retain the 256 KiB ceiling. Legacy history retains its existing bounded-file
limit. Over-limit or incomplete evidence is unavailable, never a partial
success. Deleting or restoring history can remove evidence; preserve the
adapter's own durable key and treat absence conservatively.

The external `adapter_outcomes` fixture kills a real facade consumer before
admission, after admission, after commit but before Desktop event delivery, and
after the adapter consumes the delivered result.
Additional protected-store tests cover output lineage, cache eviction,
backup/restore and revoked readers. All fixtures use temporary homes and local
synthetic providers; they do not qualify third-party delivery systems.
