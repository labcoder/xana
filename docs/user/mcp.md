# Model Context Protocol catalog and compatibility

Xana implements a bounded client-side protocol and catalog foundation for
[Model Context Protocol 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28).
The owned stdio and modern Streamable HTTP transports are implemented, but they
are not yet user-configurable: profile/configuration and application
integration arrive in the next MCP slice. Tool execution, resource reads,
prompt activation, and Xana's local MCP server are also introduced by
subsequent interoperability work.

## Compatibility boundary

Xana advertises and accepts exactly MCP `2026-07-28`. That version uses
`server/discover` plus protocol, client-info, and client-capability metadata on
every request. It does not use the older `initialize` / `initialized` handshake
or an `Mcp-Session-Id`. A server that does not advertise the exact supported
version is reported as incompatible; Xana does not silently downgrade.

The private JSON-RPC adapter currently models:

- discovery and the tools, resources, resource-template, and prompts catalogs;
- bounded pagination and cache hints;
- list-change, progress, and cancellation notifications; and
- complete results and typed, sanitized peer errors.

Sampling, elicitation, MCP Tasks, legacy SSE, third-party extensions, and
extension-driven `input_required` exchanges are outside the current contract.
Unsupported messages fail closed.

## Identity and exposure

A configured server has a stable Xana name and an exact configured-identity
digest. Tool identity is always server-qualified:

```text
mcp.<server>.<tool>
```

Display titles are aliases only. They never replace qualified identity.
Duplicate names inside a server, native-name collisions, malformed ASCII tool
names, confusable names, and identity changes between catalog review and exact
lookup are rejected.

Each profile selects servers and separately allowlists exact tool names,
resource URIs, resource templates, and prompt names. A discovered primitive is
not authority: items absent from the relevant allowlist are not indexed or
loaded. Prompts are user-controlled content, never ambient instructions.

## Progressive catalog bounds

Xana retains small sanitized summaries in deterministic indexes and retrieves a
detailed tool schema only for an exact, allowlisted lookup. Default in-memory
limits include 64 servers, 2,048 tools, 2,048 resources, 1,024 resource
templates, 1,024 prompts, 64 pages per primitive, 256 items per page, and
bounded metadata and JSON Schema sizes. Truncation is deterministic and is
reported rather than silently expanding model context.

Remote descriptions and errors are untrusted. Control and bidirectional text
characters are replaced before display or model exposure. JSON Schema is
bounded by bytes, depth, and node count; network `$ref` values are rejected and
never dereferenced.

## Readiness states

Xana keeps `disabled`, `unavailable`, `incompatible`, `unhealthy`,
`not authorized`, and `ready` distinct. This prevents a disabled connection,
network failure, protocol mismatch, health failure, or profile-policy decision
from being presented as the same generic error.

This document describes the implemented protocol/catalog boundary only. It
will be extended as application integration becomes usable.

## Stdio process boundary

The stdio adapter launches one exact executable with an argument vector and an
absolute working directory. It never builds a shell command. The child receives
a cleared environment containing only a small platform bootstrap set plus
explicitly configured entries. Explicit environment values and sensitive
arguments are redacted from diagnostics.

Stdout is newline-framed JSON-RPC only. Stderr is drained independently, capped
at 64 KiB, represented as byte counts/truncation, and never parsed as protocol
or inserted into a prompt. Frames are capped at 1 MiB. A process accepts at
most 32 outstanding requests; its command queue holds at most 64 operations and
its frame queue at most 32 frames. Writes have a two-second deadline; requests
default to 30 seconds. Progress, cancellation, invalid response IDs, malformed
frames, partial frames, process exit, timeout, and queue pressure remain typed.

Xana owns the child and its process group. Dropping the final client, explicit
shutdown, cancellation, a crash, or a protocol failure starts cleanup. Closing
stdin gives the peer a two-second graceful deadline by default; Xana then kills
the owned process tree. Restart is an explicit fresh spawn and never replays a
request. Health distinguishes starting, discovering, ready, degraded,
stopping, stopped, and crashed states.

Until configuration is wired, there is no supported command to start this
transport. If internal conformance fixtures fail, the actionable categories are
spawn/pipe failure, invalid protocol, timeout, queue/full outstanding limit,
and process exit; peer stderr content is deliberately not echoed.

## Streamable HTTP boundary

Xana implements the current stateless Streamable HTTP shape: each JSON-RPC
request is one HTTP `POST`, with either one JSON response or a request-scoped
SSE response containing bounded progress followed by the final response.
Closing that response stream cancels the request. Xana does not open a legacy
SSE event endpoint, retain an `Mcp-Session-Id`, reconnect a transport session,
or replay an interrupted paid/effectful request.

Every request sends the pinned protocol version, exact method, conditional
primitive name, and the same client metadata carried by the JSON-RPC body.
Approved `x-mcp-header` tool-schema fields may project primitive string,
integer, or boolean arguments into bounded request headers; null values are
omitted and unsafe bytes use the protocol's sentinel Base64 form.

HTTP endpoints are exact identities. Production endpoints require HTTPS,
reject embedded credentials, fragments, redirects, proxy inheritance, and
private/link-local/special DNS results, then pin the validated resolution for
the request client. The endpoint, protocol version, OAuth issuer, and client ID
all participate in the outbound recipient digest. Changing any of them makes a
saved approval inapplicable.

## OAuth ownership and local completion

For servers that advertise OAuth, Xana follows the `WWW-Authenticate`
protected-resource metadata link, validates the exact protected resource and
authorization-server issuer, requires PKCE S256, and binds token requests to
the resource. Ambiguous issuers require explicit selection; redirects and
unsupported metadata fail closed.

Xana can bind a temporary loopback callback on `127.0.0.1`, show the
authorization URL for a browser or manual/headless opening, validate callback
path, state, PKCE, and issuer, and then close the listener. No hosted Xana
backend is required. MCP itself does not define a universal device-code flow,
so Xana does not invent one. This implementation accepts pre-registered client
identities; unsupported dynamic registration metadata is reported rather than
silently changing clients.

Access and refresh tokens are stored together as one provider-scoped value in
the operating-system credential store. TOML and portable project files contain
only a stable credential reference plus non-secret issuer/client/resource/scope
metadata. Refresh is serialized by an in-process mutex and a bounded
cross-process file lock; a rotated credential is persisted completely before
its access token can be used. Corrupt/unavailable storage, revoked refresh,
insufficient scope, rate limiting, and transport failure remain distinct and
never fall back to an unrelated environment credential.

The application commands for configuring, logging in, inspecting, and logging
out of an MCP connection land with the next integration slice. Until then,
there is deliberately no supported hand-written TOML recipe or partial setup
path for these internal transport types.
