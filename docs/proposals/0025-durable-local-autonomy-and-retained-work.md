# Durable local autonomy and retained work

> Audience: Contributors and coding agents
> Authority: Prescriptive
> Status: Accepted

## One owner and explicit lifecycle

Extend the existing local attach-or-own execution host, not a scheduler in each
client. Detached background operation and OS startup are separately enabled.
Close-client detaches; close-and-stop names affected work/clients, stops new
admission and requests safe stopping with interrupted/uncertain receipts.
Controller leases still govern active human Conversations.

Each durable intent declares trigger, typed action, Project/workspace/Profile,
authorized route, grant ceiling, budget, expiry, retry policy and completion
destination. Scheduled tasks own inspectable Conversation/worker identities,
not whichever Conversation is focused. A human may inspect, pause/cancel or
explicitly take control without competing root owners.

Recheck recipients, data classes, filesystem identity, sources, credentials,
grant expiry and budget at wake and immediately before effects. No attached
human means new authority waits; memory cannot manufacture permission.
[Protected storage](0024-encrypted-managed-content-and-recovery.md) and durable
accounting gate unattended content-bearing work.

## Accounting and triggers

Reserve shared budgets atomically before dispatch; settle from attributable
receipts. Unknown usage remains unknown/conservatively reserved, not zero.
Keep estimated/reported usage and cost, rate limits, subscription quota and
prepaid credit distinct. Attribute Project, Conversation, Run, child/job, route,
connection/model/reasoning and native/managed owner. Restart or child spawning
must not reset allowance or duplicate charges.

Start with one-shot/recurring schedules, selected file changes and named GitHub
CI status. Deterministic watches/polls debounce/deduplicate; models do not poll.
File events are evidence, not instructions. Respect API limits and named-resource
credentials. No account crawling, broad inbound webhooks or ambient OS monitoring.

Use stored time zone/occurrence and injected clocks. Schedule repeated DST
wall-clock occurrences once; move nonexistent ones to the next valid instant
with a receipt. Coalesce missed read/status/reminder events to one useful current
evaluation; expire stale intents. Never retry uncertain external effects without
supported reconciliation evidence or human review. Bound backoff and notifications.

Foreground work takes priority. Start with one background model job, 8,192 total
input/output tokens and 120 seconds/job, 32,768 tokens/day; narrower root limits
win. Coalesce by source/revision with at most 1,000 pending entries and visible
backpressure. Never silently drop requested corrections. Yield at safe boundaries,
not repeated paid-call cancellation. Provider overshoot/unknowns stay visible;
admission limits are not a promise about an external bill.

Espejo and all existing clients expose real upcoming/running/paused/needs-you/
failed work, exact pause/resume/cancel, grants, usage and outcomes. Notifications
are redacted, deduplicated projections, never authority.

## Retained workers

Persist identity, goal, selected immutable evidence, lineage, grant ceiling,
cumulative budget, cancellation identity and receipts, not an idle process or
interpreter heap. Follow-up is a new bounded execution after current checks.
Preserve native/managed ownership and one-generation orchestration limits.

Exchange bounded status/summary/artifact references by default. Provide native
search/slice/filter/map/reduce/derive/cite operations over immutable context with
aggregate byte and explicit model-call accounting. A reducer is not hidden
arbitrary code execution. Recursive interpreters, persistent kernels and
automatic Skill/policy rewriting require later acceptance.

## Implementation and scope

Local host/controller/shutdown coordination and bounded one-turn children exist.
Protected homes now have [restart-safe admission accounting](../user/usage-budgets.md)
for native requests, focused services and managed outer turns, including child
inheritance, conservative unknowns and shared day/root/job limits. This does not
implement the background scheduler or impose a vendor-side billing ceiling.
Durable autonomous scheduling and retained-worker continuations do not yet
exist. Remote hosts, tenancy, messaging and universal
computer control are not included. Prove restart, cancellation, duplicate events,
clock changes, forgotten sources and unknown-effect recovery before promotion.
