# Model Context Protocol catalog and compatibility

Xana implements a bounded client-side protocol and catalog foundation for
[Model Context Protocol 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28).
The owned stdio and modern Streamable HTTP transports and the client
application layer are implemented. Profiles select exact servers and maintain
separate allowlists for tools, resource URIs, resource templates, and prompts.
The local Xana MCP server is a separate surface described later in this guide.

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

## Configure profile exposure

MCP is opt-in at three independent layers: the server declaration is enabled,
the profile selects the server, and the profile allowlists each primitive.
The server and profile must also name egress policies that allow the relevant
data classes. Discovery/list/read identifiers use `workspace_metadata`;
tool arguments and prompt-template arguments use `prompt_text`.

```toml
[mcp_servers.docs]
transport = "stdio"
command = "docs-mcp-server"
args = ["--stdio"]
enabled = true
egress_policy = "mcp"

[profiles.default]
mcp_servers = ["docs"]
egress_policy = "mcp"

[profiles.default.mcp_allowlists.docs]
tools = ["search"]
resources = ["docs://guide"]
resource_templates = []
prompts = ["review"]

[egress_policies.mcp]
allowed = ["prompt_text", "workspace_metadata"]
```

Configuration alone performs no process or network work. Use these explicit
commands to add one reviewed declaration, connect, and inspect the bounded
catalog:

```text
xana mcp list
xana mcp add-stdio docs --command docs-mcp-server --arg=--stdio \
  --profile default --allow-tool search --allow-resource docs://guide --yes
xana mcp add-http remote-docs --url https://mcp.example.test/rpc \
  --credential-env DOCS_MCP_TOKEN --profile default --allow-tool search --yes
xana mcp refresh docs
xana mcp tools docs
xana mcp resources docs
xana mcp read docs docs://guide
xana mcp prompts docs
xana mcp prompt docs review --arg text="review this"
xana mcp remove docs --yes
```

`add-stdio` stores an exact executable and argument vector; it never constructs
a shell command. `add-http` stores an exact endpoint and optional environment
credential reference, never the credential value. Both enable the declaration,
select it in exactly one profile, and grant only the repeated primitive names
listed on the command. An empty allowlist grants nothing. They validate and
atomically replace the complete configuration, retain `config.toml.bak`, and
do not start a process or request until a later explicit refresh or use.
`remove` deletes only the declaration and its profile references.

`refresh SERVER` prints the exact content-free recipient review, connects only
after that explicit action, and saves the recipient/`workspace_metadata` grant.
This grant lets future conversations discover the allowlisted catalog without a
pre-TUI prompt. Until refresh establishes it, profile activation skips that MCP
server instead of starting a process, contacting an endpoint, or preventing the
rest of the conversation from opening. Inspect or revoke the grant with
`xana outbound list` and `xana outbound revoke`.

The same command family is available as `/mcp ...` in plain chat and the TUI;
Xana restores the conversation after the typed operation. Starting a native
conversation activates only allowlisted MCP tools from servers with an exact
saved discovery grant. Exact schemas load on demand and remote tool execution
shows the outbound gate's redacted destination/item review in the normal Xana
permission surface before transport I/O. Saving that separate `prompt_text`
decision avoids redundant later prompts for the same recipient and class; a
saved denial remains authoritative.

Resource reads and prompts are intentionally not tools. `read` returns an
attributed `untrusted = true` document only after an explicit action. `prompt`
returns a preview with only user or assistant messages; a server-supplied
system role is rejected. Neither is inserted ambiently into Xana's system
prompt.

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

If a stdio connection fails, the actionable categories are
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

The MCP command family resolves credentials already referenced by
configuration. OAuth tokens remain in the OS credential store; no CLI output
includes them. `xana doctor` inspects declarations and profile exposure without
starting a process or network request; live server catalog checks remain the
explicit `xana mcp refresh SERVER` action.

## Local Xana MCP server

Xana can expose one deliberately small local surface to another MCP client:

```text
xana mcp serve --workspace C:\work\project --profile automation --allow xana_docs
```

On macOS or Linux, use the same command with an absolute POSIX workspace path.
The command speaks newline-framed JSON-RPC on stdin/stdout. Configure it in the
client as a local stdio MCP server with the exact arguments shown above. The
current server exposes only `xana_docs`; repeatable `--allow` exists so future
bounded primitives can use the same explicit contract, but unknown names fail
before the protocol loop starts.

The workspace must exist and canonicalize, the profile must exist and not be
archived, and the profile must select `xana.docs.read` when it has an explicit
capability list. Because no interactive controller is attached, the effective
profile permission mode must be `allow`, and any matching `ask` or `deny`
permission rule still wins. Failure occurs before a tool or provider runs.

Each invocation is an isolated process with an immutable workspace, profile,
and allowlist. It cannot see or control Xana's active conversation, frontend,
session list, provider, or ambient approval state. Calls receive bounded start
and finish progress notifications, complete within the ordinary 1 MiB frame
limit, and can be cancelled with a correlated cancellation notification. EOF
cancels outstanding calls, shuts down the permission broker, and gives owned
tasks a bounded cleanup deadline. The server opens no socket and does not run
as a daemon.

The local server is a private integration surface pinned to MCP `2026-07-28`,
not a stable remote Xana API. It intentionally has no inbound OAuth, network
listener, discovery broadcast, multi-user state, ambient session access, or
model-backed execution today.
