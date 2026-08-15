# Phase 3 composition services

> Audience: Contributors and coding agents
> Authority: Descriptive

Xana is one Cargo application package. The capability module owns validated
capability and logical-tool identities plus their immutable snapshot; the
headless agent loop and process composition remain separate modules within the
same package. The application owns configuration, policy, persistence,
provider and managed-runtime adapters, frontend adapters, and built-in effects.
The package boundary is not a promised public Rust SDK.

The capability resolver separates installed descriptors, enabled/selected
capabilities, dependency availability, logical tool conflicts, and the final
immutable snapshot. Production built-ins are first resolved as capabilities;
only the resulting logical tool names are instantiated in the runtime registry.
Authorization remains an invocation-time permission-broker decision and is
never implied by discovery.

The base production snapshot currently exposes `read_file`, `list_files`,
`edit_file`, `run_command`, `read_document`, and `xana_docs` in deterministic
order. Adding or removing a capability requires a new agent composition; a
model's schema does not mutate during a native turn.

Profile-exposed `image.generate` routes add one `generate_image` tool. Planning
resolves an exact route, recipient identity, and prompt data class. Execution
passes the actual external-effect authorization into `OutboundGuard`; only the
guard-owned send seam may resolve credentials, construct the adapter, or open
the network. Generated binary output is published to the artifact store rather
than copied into the transcript.

`self_docs` is a curated `include_str!` catalog with logical ids, audience,
authority, lifecycle, topics, product version, traversal rejection, and an
independent 32 KiB read limit. The system prompt lists available logical ids
but does not inject all documentation. `xana_docs` performs bounded list/read
operations so users can ask Xana about its own configuration and architecture.

Document extraction is content-first and bytes-based. `read_document` performs
one checked, bounded workspace read, passes bytes rather than a path to the
extractor, and returns bounded JSON. The default feature supports UTF-8 text
and CSV-to-Markdown with independent input, cell-work, and output limits.
Structured formats not implemented by this build fail explicitly; the parser
does not execute helpers, follow links, perform network access, or persist
secrets.
