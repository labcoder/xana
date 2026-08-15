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

Xana binds that decision to the complete redacted review. If the recipient,
purpose, class set, item metadata, byte counts, or content digests differ when
the guarded send begins, the request is denied before transport and no saved
grant is written.

Saved decisions are private machine-local state in
`data/interoperable/outbound-decisions.json` beneath Xana's platform data
directory, or beneath `XANA_HOME/data/` when that override is active. They are
bounded, atomically replaced, and cross-process locked. Inspect and revoke them
without exposing selected content:

```console
xana outbound list
xana outbound revoke IDENTITY_DIGEST selected-artifacts --yes
```

They match one stable recipient
identity and one data class. A changed endpoint, Agent Card identity, service
identity, or executable identity therefore requires a fresh decision.

The content-free authoritative audit journal is a separate bounded private
record at `data/interoperable/outbound-audit.json`. It retains at most 512
ordered metadata records. It is not a transcript and does not inherit the
diagnostic log retention setting.

When no interactive approval controller is present, unresolved approval fails
closed. External scopes require the exact controller path even when the broad
tool permission default is `allow`; a prior exact session grant or saved
outbound decision can satisfy the corresponding narrow review. A denial or
cancellation sends no protected payload bytes and does not resolve a credential
or construct a network client.

## Audit and privacy

Each guarded request emits typed request, decision, send, cancellation, and
result events. These records contain operation and recipient identity, classes,
item/reference counts, byte bounds, decision source, and outcome. They do not
contain prompts, message or file contents, artifact bytes, credentials,
authorization headers, or hidden model reasoning.

Request, decision, and sending audit records commit before bytes leave Xana; a
failure there sends nothing. After the transport returns, audit degradation is
reported separately and Xana preserves the real transport receipt or error so a
completed non-replay-safe effect is never presented as safe to retry.

MCP stdio and Streamable HTTP, A2A delegation, focused image generation, and
specialist vision all pass the same guard before their payload-bearing send seam.
Explicit CLI resource/prompt/catalog actions reuse the command itself as the
one-shot review action. `xana mcp refresh SERVER` additionally renders and saves
the exact recipient/`workspace_metadata` grant used by later profile activation.
Agent-chosen MCP tools first show the guard's content-free destination, classes,
items, provenance, bounds, and digests in the ordinary approval surface. In all
cases connection and profile egress ceilings still apply, saved denials win,
recipient identity is rechecked, and a denial sends zero bytes. Metadata-only
audit events are also forwarded into Xana's bounded diagnostic sink; there is
no transport-specific bypass or discarded production audit vector.
