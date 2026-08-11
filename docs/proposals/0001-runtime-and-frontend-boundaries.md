# Runtime and frontend boundaries

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

[Architecture](../architecture/README.md) describes Xana's implemented
application package and logical boundaries. This proposal explores the broader physical
and hosting topology that keeps the engine headless, puts application policy
in one runtime layer, and treats every frontend as a client of that layer.

The single application package and foreground command/event boundary have
been implemented through current Architecture and historical
[Proposal 0010](0010-foreground-runtime-protocol.md). This proposal remains
Proposed for the broader package topology, shared capability catalog, and
additional frontend shapes described below. The exact local frontend and
workspace-host subset required for Phase 5 is accepted separately by
[Proposal 0017](0017-bounded-local-frontends-and-workspace-host.md); accepting
that subset does not give authority to the broader designs here.

## Proposed component model

```mermaid
flowchart TB
    CLI["CLI / TUI"]
    GUI["Desktop / Web"]
    CLIENT["Supervising client"]

    RUNTIME["Application runtime<br/>configuration · sessions · threads · coordination<br/>capabilities · permissions · artifacts"]
    CORE["Headless engine<br/>agent loop · conversation · tool contracts<br/>events · context budgets"]
    CATALOG["Capability catalog and resolver"]

    PROVIDERS["Conversational provider adapters"]
    TOOLS["Tool registry and injected executor"]
    SERVICES["Focused services"]
    EXECUTORS["Execution backends"]
    STORES["Durable stores"]

    CLI <-->|"commands / events"| RUNTIME
    GUI <-->|"commands / events"| RUNTIME
    CLIENT <-->|"commands / events"| RUNTIME

    RUNTIME -->|"typed configuration and commands"| CORE
    RUNTIME --> CATALOG
    CATALOG --> PROVIDERS
    CATALOG --> TOOLS
    CATALOG --> SERVICES
    CATALOG --> EXECUTORS
    CORE --> PROVIDERS
    CORE --> TOOLS
    RUNTIME --> SERVICES
    RUNTIME --> EXECUTORS
    TOOLS -->|"authorized invocation"| EXECUTORS
    TOOLS -->|"capability tool"| SERVICES
    RUNTIME --> STORES
```

### Headless engine

The proposed headless engine boundary owns:

- the agent loop as a value;
- internal message, content, tool-call, and event types;
- conversational-provider and focused-tool contracts;
- child-agent orchestration primitives;
- context assembly and token budgets.

It does not load configuration files, inspect environment variables, persist
sessions, grant permissions, choose operating-system paths, or render a
frontend. Tool implementations and executors are injected rather than obtained
through ambient process authority.

### Application runtime

The proposed application-runtime boundary owns:

- resolving and validating shared configuration;
- capability discovery, dependency resolution, and immutable per-agent tool
  snapshots;
- provider connections, model descriptors, profiles, and task routes;
- provider, focused-service, tool, and execution-backend construction;
- permission evaluation, approval correlation, and audit facts;
- artifact storage and resolution;
- project, thread, turn, operation, and agent identity;
- persistence, recovery, admission, cancellation, and concurrency limits;
- command routing, client snapshots, and event fan-out.

A foreground CLI could embed the runtime. A future `xana serve` process could
host the same runtime for attached clients. This proposal does not promise a
global daemon; cross-process ownership and locking need an explicit design
before independent processes can safely write one Xana home.

### Frontends

Frontends translate interaction into runtime commands and render runtime
observations. Terminal colors, TUI layout, desktop panels, themes, window
state, and shortcuts remain frontend concerns. A frontend may edit shared
configuration through runtime-owned validation, but it does not independently
mutate shared state or implement another agent loop.

The broad command, event, snapshot, and concurrency designs are developed in
[Proposal 0005](0005-runtime-protocol-threads-and-concurrency.md). The bounded
local contract that implementation must now follow is Proposal 0017.

## Broader proposed physical workspace

The current implementation is one Cargo application package named `xana`.
Its headless agent, application policy, provider/managed adapters, local
frontend contract, plain terminal, and TUI are separate modules, not public
crate contracts. That shape is intentionally honest about the one product and
its current consumers.

A broader topology may extract a headless engine, application runtime, and
individual frontend packages after a second real frontend proves the smallest
shared interface. Package names are not predetermined by this proposal.
Extracted frontends would consume the runtime protocol rather than provider
wire types or direct session mutation. Focused bundled capability packs may
become additional crates or adapters registered at a composition root; the
engine would not depend outward on document, media, MCP, WASM, or service
implementations.

## Open questions

- Which exercised module seams are strong enough to become public crate
  boundaries?
- Does `xana serve` belong in the CLI package or a separate host package?
- What process owns shared state, locks, and migrations when more than one
  frontend attaches?
- Which runtime types form a stable cross-process protocol rather than an
  internal Rust API?
