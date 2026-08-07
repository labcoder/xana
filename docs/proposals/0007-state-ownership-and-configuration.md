# State ownership and configuration

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

As Xana gains sessions, artifacts, extensions, permissions, and frontends, its
state needs explicit owners and lifecycles. This proposal extends the existing
platform path and versioned-TOML foundations without treating one directory or
configuration file as a bucket for every kind of state.

The version 1 extension for user-owned permission defaults and rules is
accepted in [Proposal 0011](0011-scoped-runtime-permissions.md). This proposal
remains Proposed for extension, route, secret, durable-state, and structured
configuration-editing ownership described below.

## Proposed state ownership

| State | Platform location | Portable-home location | Owner | Lifecycle |
|---|---|---|---|---|
| Shared configuration | Platform configuration directory | `config.toml` | User authors; runtime validates | Durable and portable |
| Sessions and shared data | Platform data directory | `data/` | Runtime | Durable |
| Artifacts and audit records | Platform data directory | `data/` | Runtime | Declared per artifact/record |
| Context views, plan values, and harness revisions | Platform data directory | `data/` | Runtime | Versioned; durable or derivable by contract |
| Caches, downloads, and indexes | Platform cache directory | `cache/` | Runtime implementation | Disposable |
| Locks, sockets, and process state | Platform runtime directory or explicit fallback | `run/` | Active runtime | Ephemeral |
| Secrets | OS credential storage or explicit environment references | Not redirected | Credential provider | Sensitive |
| Frontend preferences | Frontend-owned application storage | Not redirected | Owning frontend | Durable and implementation-specific |

Installed extensions and runtime-owned artifacts are shared durable data, but
features introducing them define their exact directory contracts.
`XANA_HOME` remains an explicit backend portability override resolved at the
application boundary; it does not justify hidden environment access inside the
engine.

## Proposed configuration model

Shared configuration remains human-authored, versioned TOML. It may grow to
hold:

- named provider connections;
- optional model metadata overrides;
- agent profiles and named task routes;
- enabled capabilities;
- user-owned permission policy;
- extension declarations.

It does not hold plaintext credentials, session history, artifacts, caches,
audit logs, or frontend-private state.

Installed packages live in runtime-owned durable data. Enablement and profile
selection are declarative inputs; availability is resolved from those inputs
and platform health. Installation, enablement, selection, exposure, and
authorization remain separate operations.

Project-local policy may restrict user-owned authority but cannot silently
grant more. An extension manifest may declare requirements, but declaration is
not approval. When Xana gains structured configuration editing, it preserves
comments and human organization instead of serializing a typed value over the
user's document.

## Boundary formats

- TOML for shared human-authored configuration;
- JSON for network and process protocols;
- JSONL or another explicitly designed append format for conversation,
  operation, and audit records;
- Markdown for prompts, skills, and documentation;
- frontend-owned formats for private UI state.

Live events are not themselves a durable log. Snapshots derive from committed
conversation, operation, audit, and runtime state.

Capability lifecycle and routing are developed in
[Proposal 0002](0002-capability-model-and-routing.md). Durable record semantics
are developed in [Proposal 0004](0004-durable-operations-and-recovery.md).
External context is developed in
[Proposal 0008](0008-artifact-backed-context-and-native-plans.md), and harness
revision/promotion state in
[Proposal 0009](0009-evaluated-harness-refinement.md).

## Open questions

- Which named routes, permission rules, and extension declarations belong in
  the next configuration version?
- Which credential providers are supported on each platform?
- What process owns locks and migrations for shared state?
- Which derived context values are retained, garbage-collected, or regenerated?
- Which structured editor can preserve TOML comments and user organization?
