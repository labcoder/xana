# External A2A agents

Xana can discover and explicitly trust remote agents that advertise the pinned
[A2A 1.0](https://a2a-protocol.org/v1.0.0/specification) contract. An external
agent is not a Xana profile, conversational model provider, native child, or
managed Codex runtime. The remote endpoint owns its loop, history, tools, and
reasoning; Xana owns local endpoint trust and, once delegation is enabled, the
bounded handoff and visible protocol results.

## Declare, inspect, and trust

The configured `endpoint` is the exact Agent Card URL. Public deployments
normally use `https://HOST/.well-known/agent-card.json`.

```text
xana external-agent add research \
  --endpoint https://agent.example/.well-known/agent-card.json \
  --credential-id a2a-research \
  --egress-policy research
xana external-agent refresh research
xana external-agent show research
xana external-agent trust research --yes
xana external-agent list
```

`add` writes only the declaration and performs no network request. `refresh` is
the explicit fetch boundary. It requires HTTPS, rejects redirects, inherited
proxies, credentials in URLs, and public names that resolve to private or
special-use addresses. Responses must be `application/json` and at most 512
KiB. The credential reference, when present, is resolved from the named
environment variable or OS credential store and sent as Bearer authorization;
the secret is never copied into config, Card cache, output, or prompts.

Xana accepts a Card only when it has an A2A `1.0` JSONRPC interface, supports
`text/plain` input and output, has no required extension, and does not require
push notifications or an authenticated extended Card. Unsupported
authentication fails as incompatible. Card strings, skills, interfaces,
security declarations, nesting, and collection sizes are bounded before a
normalized cache is written.

`show` is offline and displays sanitized cached metadata plus the semantic
identity digest. `trust --yes` binds exactly that digest. A changed configured
Card endpoint, Card owner/name/version, task interface, protocol, capability,
content mode, security, or skill declaration produces `review_required` and
blocks profile readiness until a new refresh and explicit trust. A successful
fetch proves compatibility only; it never grants permission to send data.

```text
xana external-agent untrust research
xana external-agent remove research --yes
```

`untrust` preserves the declaration and cache for review. `remove` refuses a
connection still referenced by a profile, then removes both declaration and
private cached state. Equivalent `/external-agent ...` commands are available
from plain chat and the TUI command palette; Xana restores the conversation
after the typed operation.

## Storage and startup behavior

Shared `config.toml` contains only the logical declaration, optional credential
reference, enablement, and egress-policy reference. Normalized Card metadata,
refresh time, current identity, and trust binding live in Xana's private
`data/interoperable/external-agents.json` record. It contains no credential.

Generic startup and project/profile discovery never fetch a Card. If no profile
selects an external agent, profile readiness does not even open or create the
private A2A record. A selected missing, disabled, changed, or untrusted agent is
reported as not ready with exact refresh/trust remediation.

## Delegate one bounded task

A profile-selected, ready agent contributes one qualified tool named
`a2a__NAME__delegate`. The model can propose that tool, but ordinary tool
permission and outbound-data approval still happen before Xana opens a network
connection. The call must supply one task and may explicitly select only:

- prior message text in `selected_messages`;
- workspace-relative regular files in `selected_files`;
- immutable Xana artifact records in `selected_artifacts`; and
- the canonical workspace name/path in `include_workspace_metadata`.

Xana does not infer selections from the conversation and never sends the whole
transcript, hidden reasoning, credentials, environment, directory tree, or
ambient session state. Connection policy, user policy, and profile policy can
only narrow the allowed data classes. A denied approval sends zero request
bytes.

The remote endpoint owns its task loop. Xana streams bounded A2A status,
messages, and artifacts into the same typed activity feed used by plain mode
and the TUI. Returned content stays attributed and untrusted. Inline text,
JSON, and base64 byte parts become immutable, content-addressed Xana artifacts.
Public HTTPS artifact URLs without credentials, fragments, or query strings
remain safe external references; Xana does not fetch them implicitly.

Xana retains only bounded task identity/status metadata so interrupted work is
visible and cancelable:

```text
xana external-agent tasks research
xana external-agent cancel research TASK_ID --yes
```

Dropping an in-flight delegation triggers a best-effort remote cancellation.
If cancellation cannot be confirmed, Xana reports and records the task as
`detached_unknown`; it never claims that the remote effect stopped. Removing an
external-agent declaration also removes its local Card and task records, not
the remote agent's own history.

Trust does not silently send a prompt, transcript, workspace, file, artifact,
or metadata to the endpoint. It establishes recipient identity only; each
delegation still passes the ordinary permission and outbound-data gates.
