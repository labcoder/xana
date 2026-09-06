# Bounded local browser authority

> Audience: Contributors and coding agents
> Authority: Prescriptive
> Status: Accepted

## Product and topology

Deliver a usable optional local browser adapter under Xana's existing policy,
not another browsing agent. Prefer structured APIs/connectors when suitable.
A dedicated fresh browser supports explicit viewing/takeover/manual login,
typed navigation, structured observations, clicks/forms and bounded screenshots.
Browser capability is not required for ordinary startup or chat.

Evaluate pinned agent-browser first as a process candidate. Verify discovery,
version, optional installation, licenses/native dependencies and real behavior
on Windows x64, macOS ARM64/Intel and Linux x64 glibc. No silent download or
browser launch at ordinary startup. If required gates fail, record a no-go and
compare a smaller alternative; language or README claims are not proof.

Native macOS/Linux runs are deferred until integrated release acceptance and
do not block Windows-first implementation. This changes validation timing only:
local transport, containment, cancellation, no-replay and lifecycle failures
still prevent adopting an unsafe adapter. Unrun native checks are not passes.

## Authority and isolation

The adapter exposes typed operations, never arbitrary eval/CDP/Python/shell flags.
Reject unsupported containment modes. No everyday-profile/cookie import,
persistent authenticated-session promise, remote service or full OS/pixel control
is accepted. Temporary profiles/caches/login material are a separate disclosed
boundary from Xana record encryption.

Host egress policy, untrusted page content, tool limits and OS containment are
different guarantees. Redirects, popups, subresources, downloads/uploads,
local/private endpoints and third-party requests require enforcement or explicit
refusal. An allowlist is not a firewall. Bound complete child output, processing
and process lifetime, not just displayed text after unbounded materialization.

Page text cannot grant permission. Approval binds the precise target/page state,
data/effect and scope; navigation or takeover invalidates stale targets. Clicking
does not authorize purchase, publish, send or delete. Unknown outcomes produce
receipts/review, never blind replay, including hidden retries in a dependency.

Initial tasks have at most 30 actions/5 minutes and 128 KiB structured observation
per step, with existing media bounds for screenshots and narrower task budgets
taking precedence. Cancellation cleans up only provably owned processes/profile
state; no killing unrelated user browsers.

## Proof and delivery boundary

Use controlled local read/form/screenshot fixtures, redirects/popups/download
negatives, malformed policy, stale approval, cancellation and uncertain replies.
Measure at least five cold/warm release-profile runs, package size, RSS/CPU and
idle behavior. Separate automated native evidence from manual viewing/takeover
and useful public-site checks; cross-compilation is not runtime proof.

The [first-party browser adapter](../user/local-browser.md) implements the typed
local contract for the qualified Windows build. Native macOS/Linux and human
viewing/takeover/login qualification remain required before integrated acceptance;
this proposal stays Accepted while those gates are open. The direct adapter
uses owned process containment and a mandatory recipient proxy, not an
agent-browser dependency or its unrestricted command surface. `web_fetch`
remains the lighter bounded HTTP path.
