# Model Context Protocol catalog and compatibility

Xana implements a bounded client-side protocol and catalog foundation for
[Model Context Protocol 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28).
No MCP transport is user-configurable yet. Stdio, Streamable HTTP, tool
execution, resource reads, prompt activation, and Xana's local MCP server are
introduced by subsequent interoperability work.

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
will be extended as the transport and application integration become usable.
