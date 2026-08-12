# Outbound data approvals and privacy

> Audience: People configuring or using Xana

Xana has one typed gate for data sent to an external MCP server, external
agent, or focused service. A configured integration is not blanket permission
to share a conversation or workspace. The gate distinguishes these outbound
data classes:

- prompt text;
- a bounded Xana-produced summary;
- explicitly selected messages;
- explicitly selected file contents;
- explicitly selected artifacts; and
- bounded workspace metadata.

Connection policy sets the outer allowance. User policy, the frozen profile,
and an optional conversation policy can only remove classes from that set.
Even an allowed class is not selected automatically: the concrete messages,
files, artifacts, or metadata still have to be named in a bounded request.

## Approval

A new exact recipient-and-class combination requires approval. Review shows
the connection, destination, stable identity digest, purpose, classes, selected
item labels and references, provenance, byte counts, and content digests. It
does not render the selected content. The available decisions are deny once,
allow once, save deny, save allow, and cancel.

Saved decisions are private machine-local state in
`data/interoperable/outbound-decisions.json` beneath Xana's platform data
directory, or beneath `XANA_HOME/data/` when that override is active. They are
bounded, atomically replaced, cross-process locked, and can be revoked by the
application path that owns the integration. They match one stable recipient
identity and one data class. A changed endpoint, Agent Card identity, service
identity, or executable identity therefore requires a fresh decision.

When no interactive approval controller is present, unresolved approval fails
closed. A denial or cancellation sends no protected payload bytes.

## Audit and privacy

Each guarded request emits typed request, decision, send, cancellation, and
result events. These records contain operation and recipient identity, classes,
item/reference counts, byte bounds, decision source, and outcome. They do not
contain prompts, message or file contents, artifact bytes, credentials,
authorization headers, or hidden model reasoning.

The shared guard is implemented now. The internal MCP Streamable HTTP adapter
implements `OutboundTransport`, validates the exact recipient digest again at
its payload-bearing send seam, and cannot send before `OutboundGuard` grants
that request. Agent Plugin MCP declarations remain inert, and Xana does not yet
expose an MCP, A2A, or focused-service command in this build. The later
application integrations must use the same gate; there is no independent
transport-specific bypass.
