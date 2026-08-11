# Phase 3 composition services

> Audience: Contributors and coding agents
> Authority: Descriptive

Xana is a Cargo workspace with dependency direction `xana-cli` ->
`xana-runtime` -> `xana-core`. Core currently contains only headless capability
and logical-tool identities plus their immutable snapshot; the headless agent
loop remains in runtime. Runtime owns process composition, configuration,
policy, persistence, provider and managed-runtime adapters, frontend adapters,
and built-in effects; CLI owns the installed binary facade.

The capability resolver separates installed descriptors, enabled/selected
capabilities, dependency availability, logical tool conflicts, and the final
immutable snapshot. Production built-ins are first resolved as capabilities;
only the resulting logical tool names are instantiated in the runtime registry.
Authorization remains an invocation-time permission-broker decision and is
never implied by discovery.

The production snapshot currently exposes `read_file`, `list_files`,
`edit_file`, `run_command`, `read_document`, and `xana_docs` in deterministic
order. Adding or removing a capability requires a new agent composition; a
model's schema does not mutate during a native turn.

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
