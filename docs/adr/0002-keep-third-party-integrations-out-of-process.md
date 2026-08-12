# ADR 0002: Keep third-party executable integrations out of Xana's process

> Status: Accepted
> Date: 2026-08-11

## Context

Xana needs to consume Agent Plugin bundles, MCP servers, and external A2A
agents while preserving its Rust safety, portability, lifecycle, and authority
boundaries. Loading arbitrary native libraries, scripting runtimes, or WASM
components in-process could reduce call overhead and make some extension APIs
convenient, but it would also make third-party code share Xana's address space,
allocator, threads, credentials, process lifetime, and crash boundary.

A stable in-process Rust or C ABI would constrain Xana before more than one
real integration proves the right interface. A general WASM host would still
require a large capability, resource, compatibility, and distribution design,
and would not replace process/network protocols already standardized by MCP and
A2A. Agent Plugins v1 already has a declarative package boundary through
manifests, skills, and MCP configuration.

## Decision

Third-party executable integrations remain outside Xana's process. Xana may
read and validate declarative manifests and instruction files in-process, but
executable behavior enters through an explicitly configured, supervised
process or network protocol with typed serialization, bounded I/O, explicit
environment and credentials, cancellation, deadlines, health, and attribution.

Agent Plugin v1 contributes only its standardized manifest, skills, and MCP
configuration. Xana does not add native-library hooks, a Rust plugin ABI,
embedded scripting, or arbitrary in-process WASM execution as part of
Milestone 3. MCP stdio processes are owned children; MCP HTTP and A2A peers are
external recipients governed by endpoint trust and outbound-data policy.

## Consequences

- A faulty executable integration cannot directly corrupt Xana memory or unwind
  through Xana's Rust stack. Process failure has a typed, attributable owner.
- Xana can bound frames, queues, stderr, environment, credentials, startup,
  cancellation, and shutdown without trusting extension cleanup code.
- Windows, macOS, and Linux use the same protocol boundary even when executable
  packaging differs.
- Third-party dependencies can evolve behind standardized wire contracts rather
  than a Xana-specific ABI, and Xana need not promise ABI stability.
- Calls incur serialization and process/network overhead. Catalog indexes,
  connection reuse, bounded lazy activation, and protocol batching may reduce
  that cost where measurement justifies it.
- A supervised process is not a complete sandbox. Xana must describe
  containment honestly and may add platform sandbox policy separately.
- Pure built-in Rust adapters remain in-process when Xana owns and reviews
  their code; this decision concerns third-party executable extensibility, not
  every internal module.
- A future contained WASM or other execution design requires a separate
  accepted proposal and evidence that its capability, security, performance,
  compatibility, and distribution benefits justify reversing this choice.

## Related contracts

- [Proposal 0020: Standards interoperability and external-agent boundaries](../proposals/0020-standards-interoperability-and-external-agents.md)
- [Design Principles: Keep the engine small through explicit seams](../principles.md#keep-the-engine-small-through-explicit-seams)
- [Design Principles: Separate capability, authority, and containment](../principles.md#separate-capability-authority-and-containment)
