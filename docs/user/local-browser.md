# Dedicated local browser

Xana's native agent can use an optional, disposable local browser for a bounded
task. It is not your everyday browser profile, and ordinary startup or chat
does not launch or download a browser. Protected storage is required.

The initial adapter supports the explicitly qualified Windows Edge build. An
unqualified build or platform fails closed; installing a newer browser is not
itself a qualification. Further browser-control development and macOS/Linux
qualification are deferred. Use [public search and page reading](public-web.md)
for the current cross-platform web workflow; neither requires a browser download.
Xana does not currently bundle Chromium. Human viewing, manual login and takeover
checks remain separate from automated evidence.

The qualified browser identity is Microsoft Edge **152.0.4191.66**, CDP **1.3**,
on Windows x64. Browser auto-updates may require a new qualification before
Xana accepts that installation; Xana does not silently relax the version check.

## Ask for a browser task

For example: “Open this HTTPS page in a dedicated browser and summarize its
visible content.” Prefer a structured connector or the lighter `web_fetch`
tool when that can answer the question without a browser.

The `browser` tool offers these closed operations:

| Operation | Behavior |
| --- | --- |
| `open` | Approve an exact HTTPS origin, launch if closed, navigate and return the first page observation; leave the browser open. |
| `launch` | Review up to eight exact HTTPS recipient origins, then open a fresh visible window |
| `navigate` | Open a URL at an already reviewed recipient |
| `observe` | Read bounded, untrusted page text and fresh opaque element references |
| `screenshot` | Save a bounded screenshot as a protected immutable artifact |
| `act` | Review a click or text fill against an exact current reference and purpose |
| `takeover` | Suspend automation and invalidate old references for manual control |
| `resume` | Explicitly return control and require fresh page inspection |
| `close` | Close the owned process tree and remove its verified temporary profile |

Taking a screenshot brings only the task's owned page to the front so it has a
rendering surface. It never targets an unrelated browser window and is refused
during manual takeover.

Your Profile and outbound policy must permit the request, and normal tool
approval still applies. Page text cannot grant authority. A click is not blanket
permission to purchase, publish, send or delete; review the actual target and
purpose. The action preview includes the observed target, current URL, and a
supported form's destination, method and bounded non-secret fields. Changed
targets or form state fail without automatic retries; a site can still attach
its own behavior to the dispatched event.

Password fields require manual takeover. File inputs, uploads and downloads
are unavailable. Secret-bearing, hidden-field or unsupported form submissions
require manual control rather than an incomplete automatic preview. The first
adapter does not expose arbitrary JavaScript, CDP,
shell flags, desktop-wide control or existing browser cookies.

## Inspect, take over or close

In the TUI or plain chat use `/browser status`, `/browser takeover`, and
`/browser close`. Desktop has the same controls in **Activity → Disposable
browser**. They target the current native runtime's browser, not an unrelated
Edge window. Controller authority is required. Managed Codex owns its own tools
and does not use this Xana browser adapter.

These controls do not submit a model turn. Status shows bounded lifecycle and
receipt metadata; Desktop keeps it in an expandable Activity detail, and the
TUI opens a result view without replacing an already active modal. Takeover
stops Xana's automated actions; the visible page and its scripts still exist.
Takeover may report busy while an existing typed browser call finishes. Close
cancels that transport instead of waiting for the model to finish.
Resuming automation goes through the reviewed tool path, not a client shortcut.

An uncertain action also leaves a protected review fence for its Conversation.
Closing the browser or restarting Xana does not resolve that effect. Inspect
`/browser status` and the pending review's evidence, then verify what happened
at the recipient. Only the owner can record the exact result:

```text
/browser resolve RECEIPT_ID REVISION applied
/browser resolve RECEIPT_ID REVISION not-applied
```

In Desktop, request **Status**, then choose **Review uncertain outcome**. Keep
it unresolved if you cannot establish what happened. Stale receipt/revision
reviews fail; resolution records the owner's finding and never replays the
action. The model has no tool that can clear this fence by itself.

Close is a request until cleanup succeeds. Status can show `starting`,
`cleaning_up` or pending shutdown while the owned work is being joined. Dropping
a client request does not abandon its cleanup worker; `/browser close` can
join pending cleanup again. A failed cleanup stays visible as `cleanup_failed`
and prevents silently starting a replacement task. An uncertain
action is never automatically replayed. Closing Xana's owning runtime also
shuts down this browser; merely detaching an observer is different.
Successful close waits for the browser descendants, transport tasks and proxy
connections to finish, not just for a cancellation request to be sent.

## Limits and trust boundary

Each task is limited to 30 actions and five minutes, with at most 128 KiB of
structured observation per step and existing media limits for screenshots.
The owned process tree, proxy connections, transferred bytes, CDP frames and
pending events also have independent bounds. It is one browser owner, not a
second autonomous agent or a pool of unbounded browser processes.

Requests pass through a mandatory DNS-pinned proxy limited to exact reviewed
recipient origins/ports. Redirects, popups and subresources cannot introduce an
unreviewed recipient. Local files, private endpoints and non-proxied UDP are
unavailable in the supported mode.

**Recipient restriction is not an effect sandbox or an OS firewall.** Scripts
at a reviewed recipient may contact that recipient, including via WebSockets,
without asking for each request. Review the site's trustworthiness before
opening it; do not treat a read request as proof that the site cannot change
its own state. Native qualification uses controlled negative fixtures, not a
claim to defend against a compromised browser binary or a hostile local user.

Xana encrypts retained receipts and screenshot artifacts. The browser's own
temporary cache, profile and manual login material are a separate local
boundary; they are not encrypted by Xana's record store. Cleanup deletes the
owned profile but is not forensic secure erasure. No persistent login is promised.
Abrupt process or PC failure can leave temporary profile files even though the
owned Job Object terminates browser descendants; async deletion is not a
power-loss guarantee.

Structured inspection excludes known password/file input values; automatic
form previews refuse secret-bearing fields. This is not a universal secret
scanner: visible page text, URLs and screenshot pixels may contain private
information. Screenshot encryption protects storage, not the contents of an
image you later choose to send to a model or another recipient.

See [protected storage](protected-storage.md), [permissions](permissions.md)
and [outbound disclosure](outbound-data.md) for the shared policy boundaries.
