# Evaluated semantic compaction

Xana's normal native compaction remains the deterministic, source-attributed
checkpoint extractor. An optional native helper can produce richer checkpoints,
but only after the exact connection/model passes the fixed synthetic evaluation
and you explicitly enable it. Managed Codex continues to own its own compaction;
Xana does not call a second agent to summarize Codex history.

## Evaluate and opt in

Use a disposable protected home first. Configure a native connection normally,
then authorize forty synthetic requests explicitly:

```text
xana session evaluate-compaction --connection ollama --model YOUR_MODEL --yes
```

This sends only the checked-in synthetic corpus, not your Conversations, files,
or personal memory. It can still cost money on a paid connection. Every request
uses the existing protected usage ledger; there is no silent fallback to another
provider. `--yes` authorizes this evaluation, not arbitrary future tool actions.

The report compares retained required facts and correction/scope canaries with
the deterministic baseline. The gate requires all forty cases, at least 95%
required-fact retention, every correction/scope canary, and an improvement over
baseline retention. Exact-string synthetic scoring is a reproducible smoke
measurement, not a universal measure of semantic accuracy; inspect its limits
and evaluate your own interaction quality before relying on it.

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

The helper receives no tools and does not execute historical tool calls. Its
quoted input must fit 5,120 estimated tokens, output reserves 2,048 tokens,
streamed text/reasoning is capped at 8 KiB, and a request has a 120-second local
deadline. Oversized sources retain the deterministic baseline instead of being
partially summarized. Local cancellation cannot guarantee a remote provider
stops processing or billing; unfinished usage remains reserved until known.

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

No helper is enabled merely by installing Xana or running deterministic tests.
Review [project context](project-context.md), [usage budgets](usage-budgets.md),
and [personal memory controls](personal-memory.md) for the related boundaries.
