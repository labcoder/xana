# Completion evidence

Xana records completion evidence for finite native work: one-shot requests,
scheduled tasks, child runs and retained-worker follow-ups. Ordinary interactive
chat has no implicit task-correctness score.

## Read a completion receipt

| Result | Meaning |
| --- | --- |
| Delivery verified | Xana received a nonempty result and found no unresolved failure in the observed work; it did not prove that the answer solves your task. |
| Conditions verified | Xana found current evidence for each declared acceptance condition. |
| Incomplete | Xana observed missing work, a failed check, cancellation or an exhausted budget. |
| Needs attention | Xana lacks evidence, found an uncertain effect, or could not finish its verification pass. |

You can inspect a completion summary in one-shot stream-JSON output, plain-text
one-shot diagnostics, terminal usage details and Desktop's latest receipt. Xana retains the detailed native
receipt with the Conversation journal. Scheduled and retained-worker receipts
also carry the evidence for their execution. Reconnecting does not turn an
unfinished verification attempt into a success.

## Require an observed command check

```sh
cargo run -- -p "Run cargo test --offline and report the result" --accept-command "cargo test --offline" --accept-cwd . --output stream-json
```

`--accept-command` declares a check. It does not run the command, grant tool
permission or authorize a retry. Xana requires the built-in command adapter's
successful exit status for that exact command and working-directory string.
The adapter records this status before it truncates or stores large output.

After a failed check, the agent can fix the work and run the same check again
within its existing authority and budget. A later matching successful check can
resolve the earlier known failure. A different command, a stale successful check,
or text claiming that tests passed cannot do so. Xana treats a later potentially
mutating invocation as a new work revision; a declared check must cover the
current revision.

An unresolved external effect still needs review even if a command passes. Xana
does not infer that an interrupted command or browser action had no effect.

## Artifacts and bounded verification

Xana compares registered artifact identities and content hashes. It can run at
most one local verification pass per completion generation, with a total read
ceiling of 16 MiB. It records the reservation before reading artifacts and checks
owner cancellation while the worker runs. The verifier cannot call a model,
execute a command or repeat an external effect.

A missing or replaced artifact prevents success. If the process stops after
reserving verification, Xana retains a needs-attention receipt; restarting does
not authorize another verification attempt. Deterministic context operations
reuse their existing artifact-write receipts and cumulative work accounting.

Xana caps each evidence category at 64 items and a receipt at 64 KiB. It reports
omitted observations as needs attention. It retains at most 128 recent durable
receipts while preserving active declarations, and bounds live completion
projections by encoded bytes so long conversations do not grow reconnect frames
without limit. Historical detail stays in the journal.

Receipts show the owner's known remaining allowance. Native work reads the
existing indexed usage counters for remaining requests and tokens, including
root, daily and background-job ceilings. Context work records remaining context
operations. Time and cost remain unknown where no applicable remaining balance
is available; an unavailable value is not zero usage or an unlimited allowance.
Existing execution limits still apply.

## Native and managed boundaries

Codex owns its inner agent loop. Xana can record the vendor result and the
receipts the vendor exposes, but it cannot turn vendor prose into native test
evidence. Managed one-shot requests reject `--accept-command` before starting a
vendor turn. Managed children also reject native acceptance conditions. Their
delivery receipts do not claim independent task correctness.

The typed finite-work API also accepts an exact artifact reference or a declared
condition without a supported checker. Xana leaves an unsupported condition
unverified. This implementation does not add a general evaluator, an automatic
repair loop or a separate model judge.
