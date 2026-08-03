# Xana Design Principles

These principles define the boundaries Xana intends to preserve as it grows.
Xana is a guide within the system: named for the Asturian xana, it should make
complex paths understandable without becoming the source of hidden authority.
They are constraints on implementation, not a claim that every component
described here already exists.

## Keep the engine headless

`xana-core` owns agent behavior: the loop, internal tool contracts,
conversational provider contracts, events, orchestration, and context budgets.
It accepts typed commands and emits typed events. Host side effects occur
through injected executors rather than ambient process authority. Core does
not read global configuration, persist sessions, print a CLI, or know about
desktop windows.

## Put application policy in the runtime

`xana-runtime` is the application layer around the engine. It resolves shared
configuration and named capability routes, constructs agents and focused
services, owns sessions, threads, artifacts, and permission policy,
coordinates concurrent work, and fans events out to clients.

This keeps policy out of both the engine and the frontends. A foreground CLI
may embed the runtime, while `xana serve` may host it for several clients; the
same ownership rules apply in either shape.

## Treat frontends as clients

The CLI, TUI, desktop, and web interfaces send commands and render events.
They do not implement a second agent loop or race to mutate shared session
state.

Presentation preferences belong to the frontend that understands them. A
desktop theme, panel layout, window position, or keyboard shortcut is not
agent configuration.

## Separate control, observation, and telemetry

Runtime commands, permission policy, and explicitly installed hooks are control
surfaces: they may change whether or how work executes. `AgentEvent` is an
observation surface. Event subscribers may render, log, or relay behavior, but
their success, failure, or timing cannot change the operation being observed.
Telemetry is a third, diagnostic surface and is not a source of application
behavior.

Events that report durable facts are emitted after those facts commit.
Transient streaming observations are explicitly live-only and are represented
in an attached client's snapshot while work is in progress. Events carry
typed, serializable values and references rather than live provider objects or
secrets. Approval requests and decisions remain durable audit facts even when
ordinary rendering events are live-only.

A remotely attached frontend receives an atomic state snapshot followed by a
gap-free live event stream. Reconnection obtains a fresh snapshot; clients do
not reconstruct authoritative state by hoping they observed every live event.

## Pass configuration inward as values

The runtime loads and validates shared user configuration at the edge. Each
agent receives an explicit configuration snapshot containing its provider,
model, capabilities, tools, limits, routes, and profile-derived permission
ceiling.

Core behavior must not depend on hidden environment reads, process globals, or
an ambient singleton.

## Keep one internal conversation model

Provider wire formats remain inside their adapters. The rest of Xana speaks
internal types such as messages, content blocks, tool calls, and agent events.
Supporting another provider should add an adapter rather than branch the
engine around a second conversation model.

Text and tool content ship first, but the internal model is content-based
rather than string-based so image input and other typed media can join the
same conversation. Provider adapters translate those blocks at the edge.

## Route capabilities instead of overloading providers

A configured provider is a connection and protocol boundary. A model
descriptor declares what a particular model supports: input and output
modalities, tool use, reasoning features, limits, and compatibility details.
The runtime resolves named agent profiles and task routes against those
descriptors and rejects unsupported operations explicitly.

Conversational generation, image generation, speech synthesis, transcription,
and embeddings have different request, streaming, artifact, and lifecycle
needs. They use focused interfaces even when one vendor account, credential,
or base URL serves several of them. Shared authentication is not a reason to
create a provider trait full of unrelated optional methods.

## Keep media durable by reference

Images, audio, and other large outputs are artifacts with an owner and a
lifecycle. The runtime stores them in the appropriate durable or disposable
location and gives core, sessions, tools, and frontends a typed `ArtifactRef`.
Provider adapters may encode bytes for a request or decode a response, but
provider payloads and repeated base64 blobs do not become Xana's session or
frontend protocol.

## Make effects durable without guessing

A completed transcript is not enough to recover an agent safely. The runtime
tracks stable operation, step, and tool-invocation identities separately from
model-visible conversation. Before a side effect begins, it durably records
the exact effective arguments, the permission decision, the expected result
identity, and whether that invocation may be replayed. It records the result
afterward.

Broad effect class and replay safety are independent metadata. Effect class
helps policy decide what authority is required; replay safety answers whether
the same invocation may be attempted again after an unknown outcome. Unknown
or unsafe unfinished effects are never silently repeated. Recovery records an
interrupted result and lets the model or user decide what to do next.

Opening or restoring a session is read-only. Resuming a suspended operation is
an explicit runtime command that may perform effects and therefore passes
through current authority checks. This distinction lets frontends inspect
crash state without accidentally continuing it.

## Separate capability, authority, and containment

A model or tool may be capable of an action without being authorized to take
it. Every side-effecting tool, extension, foreign agent, browser, or desktop
action crosses one runtime permission broker. User-owned policy evaluates
`deny` before `ask` before `allow`; model output, prompts, project files,
extensions, profiles, and child agents cannot increase their own authority.
A child receives no more than the intersection of its parent's ceiling and
its selected profile.

Approval requests and decisions are typed, correlated, and auditable.
Interactive clients may render them, but only an authorized controlling client
may decide. Noninteractive execution fails closed when policy requires an
approver.

Permission policy is an authorization and accident-prevention layer, not a
sandbox. Local host execution, a tool routed into a container or VM, a remote
executor, browser automation, and desktop control have different real
boundaries and must identify them honestly. Only OS- or
virtualization-enforced isolation is described as containment.

## Give every path a lifecycle and owner

Human-authored configuration, durable sessions and artifacts, installed
extensions, permission audit records, disposable caches, runtime coordination
files, secrets, and frontend-local preferences are different kinds of state.
They should not be collapsed into one file or handed to several independent
writers.

Xana uses operating-system conventions by default and offers an explicit
portable home override. The canonical application identity,
`io.github.labcoder.xana`, is durable compatibility state.

## Make concurrency structured

A runtime may host many projects, threads, and agents. One root turn mutates a
given thread at a time. Other threads and bounded child agents may run
concurrently, with explicit lineage, cancellation, depth, turn, and token
limits.

Concurrency must have an owner and a lifetime. Detached work that can silently
outlive its thread is a bug.

Durable conversation entries are immutable and may carry parent links. A
thread or future branch advances a runtime-owned head rather than copying or
rewriting its shared history. This leaves room for branchable conversations
and child-agent lineage without exposing a second public concurrency concept
before the product needs one.

## Keep the core small through explicit seams

Tools, focused service/provider adapters, event subscribers, command
producers, and context hooks are the primary extension seams. A feature
belongs in core only when an extension could not provide it without violating
safety or correctness.

Extension code may still be powerful. Extension manifests declare requested
capabilities, and extension-originated side effects cross the same permission
broker as built-in tools. Loading code and authorizing its actions remain
separate decisions.

## Treat context as a budget

History, memory, tool output, and every other prompt source pass through an
explicit token budget during context assembly. Durable stores may grow; the
automatically injected slice may not.

Between deliberate compaction or navigation boundaries, context should grow at
the tail and keep its existing prefix byte-stable when practical. This both
makes step boundaries easier to recover and preserves provider prefix-cache
value. Compaction is an explicit state transition, not an incidental rewrite
caused by an extension inserting content into the middle.

## Be cross-platform by construction

Use Rust path APIs and platform-aware libraries rather than assembling paths
as strings. Shell execution is an explicit policy: POSIX shells on
macOS/Linux, and configured Git Bash, PowerShell, or `cmd` behavior on
Windows. Linux, macOS, and Windows CI must remain meaningful.
