# Evaluated semantic compaction

Xana's normal native compaction remains the deterministic, source-attributed
checkpoint extractor. An optional native helper can produce richer checkpoints,
but only after the exact connection/model passes the fixed synthetic evaluation
and you explicitly enable it. Managed Codex continues to own its own compaction;
Xana does not call a second agent to summarize Codex history.

## Evaluate and opt in

Use a disposable protected home first. Configure a native connection normally,
then authorize forty synthetic cases (up to fifty requests) explicitly:

```text
xana session evaluate-compaction --connection ollama --model YOUR_MODEL --yes
```

This sends only the checked-in synthetic corpus, not your Conversations, files,
or personal memory. It can still cost money on a paid connection. Every request
uses the existing protected usage ledger; there is no silent fallback to another
provider. `--yes` authorizes this evaluation, not arbitrary future tool actions.

The v2 corpus includes ten two-cycle cases that feed the actual first summary
into a later correction. Qualification requires all forty cases/fifty cycles,
at least 95% of 200 required fact observations, every correction/scope canary,
and better retention than the deterministic baseline. Facts earn credit only
in active fields; historical references neither earn retention nor activate an
old target. A separate summary-plus-recent-tail score cannot qualify a helper.
These exact-value synthetic checks are reproducible but not universal semantic
accuracy: valid paraphrases can miss a string check, and passing this corpus
does not prove every future summary correct.
Likewise, a negated old target or rejected tool instruction in an active field
can trigger the conservative canary; the report does not establish semantic
adoption. Reports intentionally omit generated text, so investigating those
ambiguities requires a separate synthetic review, not relabeling a failed route
as qualified. Missing second cycles still count against the fixed 200-fact plan.

For a short diagnostic run before repeating the full suite:

```text
xana session evaluate-compaction --connection ollama --model YOUR_MODEL --case-id en-6 --yes
```

Filtered runs cannot enable a helper. Reports retain typed failures, input
estimates, separate reasoning/answer byte counts, reported usage when available,
and admission/provider/total timings. Provider error bodies, prompts, generated
answers and reasoning text are not copied into these reports. Missing usage is
not reported as zero. Run from the repository with `cargo run --locked --` in
place of `xana` if the executable is not installed.
Latest full and diagnostic attempts occupy separate protected records. An
explicit successful opt-in also retains its exact content-addressed evidence
(at most 512 KiB per approved report), so a later failed attempt cannot erase
the report cited by an existing approval. Ctrl+C settles the active evaluation
request and saves its diagnostic report without enabling anything.

To repeat evaluation and enable the exact route only if it passes:

```text
xana session evaluate-compaction --connection ollama --model YOUR_MODEL --yes --enable
```

New native launches using that exact connection and model may then use the
helper during manual or automatic compaction. Changing the endpoint, connection
configuration, model, helper protocol, or corpus invalidates the approval.
The running helper rechecks its configured recipient and approval before dispatch
and after the response; removal or modification cannot silently retain a stale
runtime authorization. The application owns configuration access, not the agent.
Provider model aliases can change remotely; reevaluate after vendor updates.

To disable it without a model call:

```text
xana session evaluate-compaction --connection ollama --model YOUR_MODEL --yes --disable
```

## Bounds, fallback, and recovery

After reopening a long Conversation, proving a new checkpoint's original
sources advances in worker pages of at most 128 records and 2 MiB, with control
returning between pages for cancellation. Every page and completed proof
recheck the exact history revision, head, privacy generation and source
eligibility. Cancellation discards preparation without changing the checkpoint;
an already-dispatched, read-only page may finish, but cannot start a helper or
commit a checkpoint.
Total proof work still grows with the selected original history; no persisted
summary or claimed digest replaces reading those originals.

The helper receives no tools and does not execute historical tool calls. Its
quoted input uses the selected model's native input budget, capped at 32,768
estimated tokens (the low-level unknown-plan fallback is 5,120). Known response
schema overhead is included in admission. The wire output ceiling is at most
2,048 tokens, narrowed by the model/output policy. Answer text has an independent
8 KiB cap; reasoning, when required by a provider, has a separate 64 KiB cap.
Each request has a 120-second local deadline.
If the adapter requested no reasoning, its first reasoning delta instead stops
local consumption immediately with `unexpected_reasoning`; Xana does not spend
the remaining output allowance waiting for a prohibited stream to finish.

Adapters explicitly send supported output caps, strict response schemas and
documented reasoning-disable controls. Unsupported options or model rejection
fail visibly; Xana never retries by silently removing these settings. An
OpenAI-compatible server's advertised dialect is not proof it enforces them;
evaluate the exact route. Normal conversation generation is unchanged.

Older tool results become UTF-8-safe 2,000-byte previews in the helper input,
with original history coordinates/artifact references retained. User messages
and corrections are not truncated. If the remaining useful source cannot fit,
the helper is refused and the deterministic candidate remains. Originals are
never modified. Local cancellation cannot guarantee remote processing or billing
stops; unknown usage is accounted conservatively.
An incoming user message plus the required tool schemas that already exceed
the input budget is rejected before compaction or a helper call. Replaceable
checkpoint prose is not included in that irreducible lower-bound check; the
complete prompt is still validated before actual model dispatch.

Malformed output, tool calls, provider failure, unavailable accounting, and
budget exhaustion cannot become successful semantic summaries. The runtime
reports the helper failure and uses its deterministic candidate when safe.
Interrupt or shutdown leaves the prior checkpoint unchanged. Source or privacy
changes reject the prepared candidate rather than applying stale results;
forgetting can exclude the entire source Conversation from automatic compaction
while leaving its original history available for explicit inspection.

Successful checkpoints keep immutable source entry IDs, ranges, source digest,
prior checkpoint identity, exact helper/evaluation provenance, and a digest of
the resulting summary. Restart validates these facts through the ordinary
session reducer. A helper cannot edit or erase the original Conversation.
Old v1 checkpoint evidence remains readable; new helper dispatch requires v2
qualification. This does not invalidate unrelated memory-processing route grants.

No helper is enabled merely by installing Xana or running deterministic tests.
Review [project context](project-context.md), [usage budgets](usage-budgets.md),
and [personal memory controls](personal-memory.md) for the related boundaries.
