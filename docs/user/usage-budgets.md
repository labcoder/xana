# Durable usage and admission budgets

Audience: Users. Authority: Descriptive.

Protected homes reserve allowance **before** each native provider request,
focused-service invocation, or managed Codex outer turn. Plain, TUI and Desktop
share the same ledger. Legacy homes retain their existing behavior; migrate
explicitly before relying on durable accounting. Ledger inspection calls no provider.

```text
xana usage ledger
xana usage ledger --root CONVERSATION_ID --job OPERATION_ID
xana usage ledger --after LAST_SEQUENCE
xana budget
xana budget --daily-requests 500 --foreground-request-reserve 32
xana budget --root-tokens 500000
```

In the repository, prefix these with `cargo run --locked --`. In plain/TUI use
`/usage ledger ...` and `/budget ...` between turns. Desktop's **Usage** panel has
Refresh, root/job filters, Next page, and labeled budget fields with Save budget.
Only edited fields are applied atomically. If a restore requires usage review,
an explicit acknowledgement button explains the unknown-charge boundary.
Read-only receipt text uses the component library's selection/copy controls.
Account/provider observations remain separate
in Activity and `xana usage`.

Pages contain at most 128 receipts: UTC day, root Conversation, operation/job,
route, connection/model, reasoning snapshot, execution owner, Profile and
Project when available. Child requests share the root allowance and inherit the
parent's job. Unavailable facts are null, never invented.

Defaults are 10,000 requests/day and 10,000/root, with 32 daily request slots
reserved for foreground work. Optional day/root token limits default to unset;
setting either to `0` removes that optional limit. Background defaults are 32,768
admitted tokens/day and 8,192/job. These are admission rules, **not an installed
scheduler**. Policy changes affect later admissions, not already dispatched work.

Token reservations are estimates: native requests use compiled prompt cost when
available; managed/focused input uses a text estimate, with a 16,384-token output
reserve. Reported per-request tokens reconcile the reservation. Missing usage
stays unknown and charged at its reserved estimate. Cancellation/crash never
implies a refund. Duplicate dispatch IDs fail; identical settlements are
idempotent. Changing policy does not erase existing reservations.

Codex reports cumulative thread tokens. Xana records these separately, never as
per-turn usage or a sum across turns; the reservation remains an estimate. A
fresh managed child owns one fresh vendor thread, so its completed thread usage
can describe that child invocation. Managed requests count outer turns, not
Codex's internal model/tool calls. Focused image counts are not tokens.

Reported cost is separate from token estimates, subscription quota, rate limits,
wallet balance and account limits. Null cost is **unknown**, not free. Local
guardrails cannot guarantee a vendor-side charge ceiling, exact tokenizer
result, or hard bound on a managed agent's internal work. Reported usage above
a reservation is recorded honestly and can block later admissions.

Restored snapshots may omit later usage. Inspect the ledger/policy, then
explicitly acknowledge that boundary with `xana budget --accept-restored-usage`.
This permits new foreground admissions; it does not enable restored memory,
learning or automation authority. Prior generations remain available for review.
Restore never resets a provider bill or quota.
