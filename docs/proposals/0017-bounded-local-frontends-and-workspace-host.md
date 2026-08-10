# Bounded local frontends and workspace host

> Audience: Contributors and coding agents  
> Authority: Prescriptive  
> Status: Accepted

## Context

Xana currently has a headless engine, a foreground application runtime, a
single append-only terminal client, durable native sessions, and managed Codex
threads. The current foreground protocol in historical
[Proposal 0010](0010-foreground-runtime-protocol.md) has one bounded command
channel and one unbounded live event receiver. It has no client snapshot,
multi-client attachment, local host, or full-screen TUI.

Phase 5 needs a polished terminal frontend and a narrow local attachment
boundary without accepting the broader daemon, remote hosting, retained work,
extension, or public-protocol designs in Proposals
[0001](0001-runtime-and-frontend-boundaries.md),
[0005](0005-runtime-protocol-threads-and-concurrency.md), and
[0007](0007-state-ownership-and-configuration.md). This proposal accepts only
that bounded local slice. Until implementation is complete,
[Architecture](../architecture/README.md) remains authoritative for what Xana
does today.

## Accepted component boundary

The application contract is a repository-private set of versioned, typed,
serializable client commands, correlated command results, redacted snapshots,
ordered events, stable identifiers, and artifact references. The ordinary TUI
uses an in-process embedded transport. A loopback adapter may project the same
semantics for local clients; it is not a second application API.

The runtime owns configuration validation, execution, permissions, sessions,
concurrency, durable truth, snapshots, sequencing, and fan-out. Frontends own
input translation, layout, rendering, terminal capability handling, and
presentation preferences. A frontend cannot implement another agent loop,
mutate durable state outside runtime commands, grant itself authority, or
receive secrets, provider-native payloads, hidden reasoning, unbounded output,
or unrestricted filesystem paths through the client contract.

`xana-core` remains free of terminal, network, environment-discovery, and
presentation dependencies. Terminal and network adapters remain outside
runtime policy. Plain and non-TTY clients consume the same application
semantics and remain permanently supported; the full-screen frontend cannot
become the only way to control Xana safely.

## Accepted workspace and conversation ownership

One host owns one canonical workspace root and any number of durable
conversations under it. Course 1 permits at most one active root turn across
the workspace. Bounded child work remains structured beneath that root. A user
may inspect another conversation and edit a frontend-local draft while work is
active, but cannot start a competing root turn through the host.

Each conversation has at most one controlling client. A controller may submit
mutating commands and answer correlated approvals. Other attached clients are
passive observers. Observation cannot grant, retry, cancel, or otherwise
change execution. A bare embedded invocation never silently takes over an
active conversation. Explicitly resuming one reports the available attach,
new-conversation, or cancellation choices.

Native conversations retain Xana-owned history. Managed Codex conversations
retain their opaque vendor-owned thread identity. The frontend may present
common navigation and activity concepts, but the runtime never translates
history or ownership between these execution models.

## Accepted attachment and security contract

`xana serve` is an explicit foreground host bound only to loopback. It does not
auto-start, daemonize, bind to LAN interfaces, or promise remote access. Local
discovery uses a workspace-scoped runtime descriptor protected for the current
operating-system user. A fresh unguessable capability authenticates attachment
without appearing in command-line arguments or shell history. Browser-capable
transports also validate the request origin. Filesystem permissions are a
defense in depth, not the only authentication check.

Attachment returns one atomic, redacted snapshot followed by a gap-free live
sequence. Events committed after snapshot capture are buffered until the
snapshot is delivered. A reconnect, sequence gap, or overflow obtains a fresh
snapshot; clients never guess state or treat the live stream as durable replay.
Command results are correlated independently of observation delivery.

A controller lease is explicit. On controller disconnect, mutations and
approval progress stop during a short bounded grace period. If control is not
re-established, pending approvals fail closed and the active root turn is
cancelled. A passive observer never inherits the lease.

## Accepted bounds and lifecycle

Every client has bounded command, result, event, and payload limits. Runtime
execution must not block on a slow observer; a client that cannot keep up is
disconnected and must reattach through a fresh snapshot. Conversation history
is paginated, rendering is virtualized, caches are bounded, and large content
crosses the contract by artifact reference with a bounded preview.

The owner maintains structured lifetimes for operations and subprocesses. An
embedded frontend closing cancels its active work. Explicit host shutdown
stops admission, requests cancellation, waits only for bounded grace periods,
escalates when necessary, reaps owned process trees, removes verified runtime
metadata, and restores terminal state. Closing an observer does not cancel
work. Already-authorized work may continue without clients only while the
explicit foreground host remains alive; it stops at a fresh approval boundary.

## Accepted presentation preferences

Presentation preferences are frontend-owned, versioned, machine-local values.
They use frontend-specific storage under Xana's platform paths and are
relocated by the explicit `XANA_HOME` override for portability and tests. They
may select presentation mode, density, pane defaults, motion, Unicode, and the
bounded composer preset. They cannot change connection, model, reasoning,
tools, permissions, commands, event meaning, or agent authority. Runtime truth
such as active work, controller ownership, unread state, and approvals is
never persisted as a presentation preference.

## Deferred designs

This decision does not accept or authorize:

- a daemon, auto-started service, LAN or internet binding, remote controller,
  multi-user host, or public compatibility promise;
- multiple workspace roots in one host or concurrent root turns in one
  workspace;
- retained or detached roots that outlive their explicit foreground owner;
- event replay as a durable log, background inboxes, hooks, or arbitrary
  frontend mutation;
- extension/package state, synchronized desktop preferences, arbitrary
  keymaps, or a general frontend plugin API; or
- translating native conversations into managed-runtime threads or exposing
  vendor credentials and wire protocols.

Those ideas remain non-authoritative unless separately accepted.

## Implementation and status

Implementation proceeds in independently verified slices: a frontend-safe
embedded client, plain and one-shot contracts, workspace host ownership,
presentation capabilities, the adaptive TUI, the authenticated loopback
projection, transactional setup, and bounded shutdown. Each slice updates
Architecture and User Documentation in the same change that ships it.

When the complete accepted contract is implemented, this proposal becomes
historical and Architecture owns the resulting description. Partial delivery
does not permit unimplemented sections to be described as current behavior.

The frontend-safe embedded-client, permanent plain/one-shot, embedded
workspace-host ownership, presentation-capability, and native adaptive-TUI
shell slices are implemented.
Native append-only chat uses
the versioned bounded client seam;
managed Codex notifications are normalized through its provider-neutral event
vocabulary; one-shot has exclusive input, stdout/stderr, JSON, exit,
continuation, and fail-closed approval contracts. A canonical workspace host
catalogs native and managed conversations, retains selectable Codex handles,
and uses an OS-authoritative per-turn gate so embedded processes cannot run
competing roots. Presentation resolves a versioned bounded preference file and
injected terminal facts into semantic dark, light, monochrome, color-depth,
glyph, width, and motion policy; redirected output remains control-free.
The loopback observer and single-controller projection is implemented: explicit foreground
`xana serve` publishes a protected per-workspace capability descriptor,
validates generation/workspace/version/capability and browser Origin before
data, then provides an atomic bounded snapshot and ordered passive stream.
Observer commands are correlated rejections and cannot reach runtime control.
One explicit controller lease routes correlated native and managed Codex turns
and exact approval decisions. Release, takeover, reconnect grace, and expiry
are sequenced host facts; controller loss fails pending approvals closed and
interrupts the root without promoting an observer. Client count/rate/frame/
queue/write bounds now isolate slow or malicious clients; authorized artifact
lookup accepts only visible immutable references and returns a verified 64 KiB
preview; the host owns two-second graceful and five-second hard shutdown of its
exact client/execution tasks. Provider-neutral Quick Setup now establishes a
live native or managed connection before model selection and atomically commits
the redacted reviewed config, backup, and rollback-safe credential reference.
Full Custom and focused sectional setup reuse that boundary for shell,
permissions, capabilities, exact profiles/routes, orchestration limits, and
presentation, with explicit immediate/subsequent-turn/new-conversation timing.
Read-only doctor, deterministic confirmed repair, validated atomic editor, and
scoped reset now share those typed path/config/provider boundaries. `/doctor`
restores the terminal around a diagnostic pause; destructive reset is exposed
in the TUI only as a guarded idle command-palette lifecycle action.
The
TUI now owns adaptive state/update/view
rendering, terminal restoration, a bounded multiline composer, safe paste,
image staging, ordered follow-ups, exact interruption, owner-gated steering,
one command registry/palette, model selection, independent activity visibility,
and bounded session rail/picker inspection with workspace-local rail
preference. Its managed Codex actor projects typed activity, exposed reasoning,
plans, commands, diffs, collaboration, reroutes, warnings and completion;
native, child, and managed approval cards retain exact correlation even when
activity is hidden. The rich-content slice now supplies bounded Markdown,
control sanitization, viewport virtualization, native history pages, inert
links, and explicit verified artifact actions.
This proposal therefore remains Accepted.
