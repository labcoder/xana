# Xana Design Principles

> Audience: Contributors and coding agents  
> Authority: Prescriptive

These principles are durable constraints and philosophies for Xana work. They
do not claim that a particular component or proposal is implemented.

## Keep the engine headless

Agent behavior accepts owned values and communicates through typed inputs and
outputs. It does not load global configuration, inspect process environment,
persist application state, render a frontend, or gain ambient host authority.
Effects enter through explicit seams.

## Put application policy outside the engine

Configuration resolution, sessions, permissions, coordination, persistence,
and client fan-out are application policy. Keep that policy outside both the
headless engine and its frontends so different hosts do not reimplement it.

## Treat frontends as clients

Terminal, desktop, web, and other interfaces translate user interaction into
commands and render observations. They do not implement a second agent loop or
independently race to mutate shared application state. Frontend preferences
belong to the frontend that understands them.

## Separate control, observation, and telemetry

Commands, permission decisions, and explicitly installed hooks may affect
execution. Observers render or relay behavior without controlling it.
Telemetry diagnoses behavior without becoming application state. A failure or
delay in observation must not silently alter the operation being observed.

## Pass configuration inward as values

Load and validate user-owned inputs at the application edge. Each agent
receives an explicit immutable configuration snapshot. Engine behavior must
not depend on hidden environment reads, process globals, or ambient singletons.

## Keep one internal conversation model

Provider wire formats remain inside adapters. The rest of Xana uses one set of
internal messages, content blocks, tool calls, results, and events. Supporting
a provider adds an adapter rather than branching the engine around another
conversation model.

## Route capabilities instead of overloading providers

Conversational generation and focused capabilities such as image generation,
speech, transcription, embeddings, and document extraction have different
contracts. Sharing a vendor or credential is not a reason to place unrelated
optional methods on one provider interface.

## Keep media durable by reference

Large media belongs to an owner and lifecycle. Internal models, persisted
records, and frontend protocols carry typed references rather than repeatedly
embedding provider payloads or base64 data.

## Treat document conversion as untrusted parsing

Structured documents are bounded, untrusted input. Type detection, archive
expansion, parser work, extracted output, artifacts, and model context require
independent limits. Conversion does not execute or automatically fetch content
found inside a document.

## Make effects durable without guessing

Recovery must distinguish conversation from execution intent and outcome.
Xana never infers replay safety from a broad effect label and never silently
repeats an unfinished effect whose outcome is unknown. Durable correctness
comes from explicit records, artifacts, and named values; a process or
interpreter heap snapshot is always disposable.

## Separate capability, authority, and containment

Being able to perform an operation does not authorize it. Authorization does
not physically contain a process. Xana names these layers honestly and keeps
user-owned authority separate from prompts, models, extensions, profiles, and
project content.

## Give every path a lifecycle and owner

Human-authored configuration, durable application state, artifacts, caches,
runtime coordination, secrets, and frontend preferences have different owners
and lifecycles. Do not collapse them into one path or hand them to competing
writers. Use platform conventions by default and explicit portable overrides.

## Make concurrency structured

Concurrent work has an owner, lineage, limits, cancellation path, and lifetime.
Admission, waiting, collection, and cancellation remain explicit operations,
and total-work budgets cover both depth and breadth. Detached work that
silently outlives its owning thread or operation is a bug.

## Keep the engine small through explicit seams

Tools, provider and focused-service adapters, event subscribers, command
producers, and context hooks are extension seams. Behavior belongs in the
engine only when an extension cannot provide it without violating correctness
or safety.

## Make optional capabilities explicit and stable

Installation, discovery, enablement, availability, selection, exposure, and
authorization are different lifecycle facts. Resolve them explicitly and give
an agent one truthful, immutable capability snapshot rather than changing its
visible tools as a side effect of use.

## Treat context as a budget

History, memory, tool output, and every other prompt source pass through an
explicit token budget. Durable stores may grow; the automatically injected
slice may not. Large working data remains addressable outside the prompt
through immutable or versioned references with provenance, trust, ownership,
and bounded materialization. Compaction is a deliberate state transition.

## Promote harness changes through evidence

Prompts, memories, skills, routes, hooks, and child definitions are versioned
inputs, not ambient self-modifying state. Keep a proposed change, declared
validation, observed held-out result, promotion decision, and rollback path
distinct. Untrusted content cannot promote itself into governing policy, and
executable or security-policy changes never auto-promote.

## Be cross-platform by construction

Use Rust path APIs and platform-aware libraries instead of assembling paths as
strings. Shell behavior is an explicit platform policy. Linux, macOS, and
Windows validation must remain meaningful.

## Applying these principles

Particular future system shapes live in [proposals](proposals/). A proposal may
apply several principles without becoming authoritative until its status is
Accepted.
