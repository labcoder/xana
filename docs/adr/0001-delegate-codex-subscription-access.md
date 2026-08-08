# ADR 0001: Delegate ChatGPT subscription access to Codex app-server

> Status: Accepted
> Date: 2026-08-08

## Context

Xana needs local ChatGPT Plus/Pro subscription access without owning an
undocumented vendor OAuth/backend protocol. Direct implementations must store
and rotate refresh tokens, reproduce product-internal request policy, and
remain synchronized with changing authentication behavior. Treating Codex as
an ordinary conversational provider would also obscure that it owns an agent
loop, tools, sandbox, approvals, and history.

## Decision

Xana launches the vendor-owned Codex app-server and uses its local JSON-RPC
contract. Codex owns login, token storage and refresh, inference, and the inner
agent runtime. Xana owns connection/model selection, process supervision, CLI
projection, and approval interaction. Xana does not read or copy Codex
credentials and does not wrap direct Codex turns in a second model call.

## Consequences

- Subscription access requires a compatible installed Codex executable.
- Authentication can complete through Codex's local browser or device-code
  flow; Xana needs no hosted OAuth callback server.
- A selected Codex turn has no automatic model-to-model token overhead.
- Native and managed execution remain distinct and history is not silently
  transferred between them.
- App-server protocol changes are an adapter compatibility concern.
- A direct Codex transport may be explored later only as an explicitly
  experimental proposal with a supported authority and a separate risk case.
