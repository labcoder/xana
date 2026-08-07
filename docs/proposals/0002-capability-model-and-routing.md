# Capability model and routing

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

Xana needs to distinguish provider connections, model behavior, agent
configuration, focused services, and optional extension lifecycle. This
proposal defines a common model without placing every vendor operation behind
one conversational provider trait.

## Proposed concepts

- A **provider connection** describes a protocol, base URL, credential
  reference, headers, timeouts, and compatibility settings.
- A **model descriptor** names a model on that connection and describes input
  and output modalities, tool and reasoning support, context and output limits,
  and optional user overrides.
- An **agent profile** selects a primary provider/model pair, tools, budgets,
  and a permission ceiling.
- A **task route** maps a named side task such as `summarize`, `title`, or
  `vision` to an agent profile or focused service.

The application runtime resolves these values before constructing an agent. A
route whose target lacks the required capability fails explicitly; adapters do
not silently remove unsupported images, tools, or other request content.

Conversational generation remains a focused provider contract. Image
generation, speech synthesis, transcription, embeddings, and similar
operations use their own interfaces even when one account or credential serves
several of them.

## Proposed capability lifecycle

| Stage | Meaning |
|---|---|
| Installed | A package or built-in implementation is present |
| Discovered | Pure metadata inspection produced a descriptor |
| Enabled | User-owned configuration permits participation |
| Available | Platform, version, health, and dependencies resolve |
| Selected/exposed | A profile selected the capability and its truthful tools entered an immutable agent schema |
| Authorized | The permission layer allowed one invocation with final arguments |

The runtime catalog is the source of truth for provider identifiers, logical
capability and tool identifiers, dependencies, lifecycle needs, and unavailable
reasons. Resolution is deterministic, detects missing requirements and cycles,
and records optional absences without silently substituting behavior.

```text
exposed = available ∩ enabled ∩ profile-selected
```

Authorization is deliberately absent from that equation because it applies to
one concrete invocation rather than capability discovery.

## Discovery and activation

Discovery and probes are pure and read-only. They do not install packages,
import untrusted extension code, spawn provider processes, or access the
network. Installation, upgrade, enablement, and removal are explicit
control-plane operations outside agent turns. Changes activate atomically and
require a runtime reload or a new agent snapshot; listing or invoking a tool
does not mutate the model-visible schema.

An expensive, already-installed MCP server, WASM module, sidecar, OCR engine,
or local model may activate on first use. Initialization should be
single-flight, cache success rather than the first result, retain typed
transient and permanent failures, and use bounded retry/cooldown policy. Lazy
activation never downloads or mutates an installation. Stateless in-process
adapters should be constructed eagerly when a lazy state machine adds no
material value.

Invocation authority and containment are addressed in
[Proposal 0003](0003-tool-authority-and-execution.md). Proposed configuration
and state ownership are addressed in
[Proposal 0007](0007-state-ownership-and-configuration.md).

## Open questions

- What descriptor fields are stable application contracts versus
  adapter-specific metadata?
- How are extension packages identified, installed, upgraded, and rolled back?
- Which health probes may use the network without violating pure discovery?
- How are capability conflicts and version compatibility reported to users?
