# Scoped runtime permissions

> Audience: Contributors and coding agents
> Authority: Prescriptive
> Status: Accepted

## Context

Xana's foreground runtime has correlated provisional approval for
`run_command`, while file tools still execute automatically. The next slice
needs one authority boundary for every built-in effect without accepting the
broader execution-backend, durable audit, remote controller, hook, or
containment designs in Proposals 0003, 0005, and 0007.

## Accepted policy model

Version 1 configuration keeps `permission_mode` and expands it to `deny`,
`ask`, and `allow`. It adds a default-empty `permission_rules` array. Existing
documents that explicitly say `allow` retain automatic host-tool authority;
new interactive initialization defaults to `ask`, and noninteractive
initialization must select a mode explicitly.

Rules have a unique nonblank id, a `deny`, `ask`, or `allow` decision, and at
least one matcher from tool name, effect class, workspace path, or exact
command string. Unknown tools and effects, unknown fields, absolute workspace
rules, and command rules with no command-capable scope are rejected. Relative
workspace rules are resolved beneath the canonical launch workspace when the
runtime is composed, not while the configuration file is parsed.

All matching decisions are combined independently of declaration order: any
deny wins, otherwise any ask wins, otherwise any allow wins, otherwise the
configured default applies. Explicit matching deny rules are evaluated before
session grants. A grant can replace only an `ask` result; it cannot override
an explicit or default deny. A pure explanation value reports matched rule ids
and the winning precedence without rule source contents.

## Accepted planning and scope model

Every tool invocation follows one registry-owned path: resolve the tool,
produce an immutable plan, request permission, emit an audit fact, and execute
only when authorized. The plan contains normalized final JSON arguments, the
canonical permission scope, and type-erased private executable data created
and consumed by the same concrete tool. The registry does not expose an
unplanned executor. No argument transformation occurs after planning; a future
hook that changes arguments must run before permission and produce a new plan.

File scopes contain the canonical target path beneath the canonical workspace.
Command scopes contain the resolved shell description, canonical cwd, and
exact command string. Lexical aliases resolve to the same scope, and a symlink
escape fails before policy evaluation. Planning may validate arguments,
metadata, and paths but performs no write, process, network, or external
effect.

## Accepted broker protocol

One runtime-owned broker task owns pure policy, session grants, pending
requests, and controller presence. Every request binds operation id, tool
invocation id, tool name, effect class, normalized final arguments, and scope.
Policy `deny` and `allow` resolve immediately. Policy `ask` emits a typed
`PermissionRequested` event and waits for a correlated control command from
the foreground terminal.

Controller decisions are deny, allow once, or allow for the current session
at the request's exact scope. Session grants are additionally bound to the
request's tool and effect, and apply only to the same or a narrower workspace
scope or to an exactly matching command scope. They are process-memory state
and never persist. Unknown, duplicate, stale, mismatched, or scope-widening
decisions are rejected. Cancellation, controller loss, runtime shutdown,
reply loss, and broker channel loss remove pending work and fail closed. An
unattended ask resolves as denial without fabricating a controller event.

The foreground terminal is the controlling client because the application
composition root creates it with the embedded runtime. This accepts no remote
authentication or client-role negotiation contract.

## Accepted audit fact

Each authorization result creates one in-memory typed audit fact containing
the stable request, policy outcome, optional controller decision, and effective
decision. Facts are observable events but do not enter conversation or prompt
context. Durable audit storage and invocation intent remain unimplemented.

## Explicit exclusions

This decision does not add persistent grants, project-granted authority,
wildcard command parsing, secrets in audit facts, hooks, remote controllers,
an execution-backend interface, durable audit or invocation records, sandboxing,
or other process containment. Approved effects still use Xana's ordinary host
permissions.
