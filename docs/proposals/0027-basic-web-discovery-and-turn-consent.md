# Basic web discovery and turn consent

> Audience: Contributors and coding agents
> Authority: None
> Status: Implemented

Implemented by the web runtime, tool adapters, permission broker and shared
client projections. See [Architecture](../architecture/README.md) and
[public web](../user/public-web.md). Deterministic protocol/authority/resource
tests, full Windows workspace gates and three consecutive local-model search
and native browser-answer cases pass. Hosted vendor availability and owner UI
observations are not inferred from fixtures. The browser remains the qualified
Windows adapter; this proposal does not close the independent cross-platform
browser qualification contract in [0026](0026-bounded-local-browser-authority.md).

## Outcome and ownership

Native Xana supplies separate search, known-page reading and dedicated-browser
tools. The selected conversational model chooses and synthesizes; the runtime
enforces readiness, disclosure, resource limits and recovery. No mandatory
router model, hosted answer generation, autonomous research service or provider
switching is added. Managed runtimes retain their own tools and policies.

Search uses a small source-evidence contract with Exa direct API, explicitly
selected Exa hosted MCP and Brave Web Search adapters. Reuse existing bounded
HTTP/MCP and credential ownership. Search connections are independent of chat
models; setup preserves unrelated configuration and doctor reports readiness.
Search mode is ordinary retrieval with bounded text/excerpts, not deep search,
generated summaries or Answers. Provider metadata and unstructured MCP text
remain honest evidence; never invent a date, source URL or usage value.

## Consent and resource contract

Exact approval stays available. A separately chosen public-web turn grant
permits bounded model-generated queries to the selected search route and public
HTTPS text reads during one owning turn. Persistent preference is separately
explicit; old exact grants are never widened and saved denies still win.
The turn grant does not allow different providers, private destinations,
credential/cookie forwarding, local files/artifacts, arbitrary HTTP/MCP calls
or browser effects. A model-generated query may disclose its contents; no
unimplemented semantic secret-detection guarantee is implied.

Default metadata logs retain origin/route, digest, counts and outcomes, not
query text or URL paths/queries. The owner-facing review and protected evidence
have different purposes and retain deliberate bounded detail.

Begin with three searches, eight outbound attempts, two concurrent requests,
16 MiB aggregate ingress per turn, 2 KiB query and 24 KiB inline search output.
Configuration may select bounded limits below compiled ceilings. Count retries,
redirects and MCP control traffic rather than only successful tools; report
unobserved provider-internal work as unknown. Search has a 25-second deadline.
Reuse eligible identical in-turn work after authority checks; no persistent
cross-turn cache or ambiguous paid-request replay.

Local fetch stays the default known-page reader: 2 MiB raw default, 4 MiB ceiling,
20-second default deadline, independent header/extraction/inline limits and
identity encoding. Support bounded UTF-8 HTML/plain/Markdown/JSON as inert data.
Every redirect is checked against live grant/deny and public pinned DNS rules;
strict exact mode still stops before an unreviewed hop. Sources retain URL,
retrieval time, truncation, digest and immutable overflow references.

## Browser and recovery

Add `browser.open(url)` as one approved lifecycle composition: check readiness,
launch a fresh owned browser if absent, navigate and return bounded observation.
Reuse only an eligible owned session; do not steal manual takeover, broaden its
origins, download a browser, reset its budgets or replay uncertain actions.
Existing explicit operations remain compatible and missing-session errors name
the correct recovery. Preserve the accepted browser authority/containment and
native qualification contract.

Typed failures distinguish missing setup, denial, authentication, rate limits,
empty search, invalid/oversized content, redirects, missing resources, challenge,
transport failure, cancellation and browser prerequisites. Bound retries and
no-progress across changing arguments. Conversation and Activity report useful
work stages and elapsed time, never fake progress or hidden reasoning. Terminal
events and cancellation are correlated to the owning operation.

## Proof

Use deterministic provider/network/browser fixtures for denial, bounds, redirects,
drift, cancellation, duplicate work and no-replay. Demonstrate complete current
fact answers and requested browser page reading using a declared local model;
separate runtime guarantees from model-specific quality. Preserve native/manual
gates and measure end-to-end cost/resource claims rather than inferring them
from dependency marketing. Hosted extraction/Answers and deeper research remain
optional future work, not prerequisites of basic web capability.
