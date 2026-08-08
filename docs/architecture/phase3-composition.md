# Phase 3 composition services

Audience: contributors and coding agents. Authority: descriptive.

The precise implemented subset and its deliberate gates are recorded in
[proposal 0014](../proposals/0014-phase3-implemented-subset.md).

Xana is a Cargo workspace with three packages:

- `xana-core` contains provider-neutral messages, stable capability/tool ids,
  image references, model capability checks, and headless service contracts.
- `xana-runtime` owns configuration, policy, persistence, document extraction,
  self-documentation, provider adapters, and the foreground runtime.
- `xana-cli` owns the installed `xana` binary facade and delegates process
  argument/environment handling to the runtime runner.

The runtime capability resolver accepts installed descriptors, enabled ids, and
selected ids. It sorts provider and tool identities, rejects duplicate
capabilities or logical tools, reports missing required dependencies and cycles,
and freezes a `xana-core::AgentCapabilitySnapshot`. Authorization is still
performed by the permission broker when a tool is invoked; discovery does not
grant access.

`xana-runtime::self_docs` is an explicit `include_str!` catalog. Logical ids,
audience, authority, lifecycle status, and topics are compiled into the binary,
and reads have independent byte and UTF-8 boundary checks. The catalog exposes
the bounded `xana_docs` core tool. It describes Xana, not the current project.

Document extraction is content-first and bytes-based. The runtime performs the
checked file read and then sends bounded bytes to `DocumentExtractor`. The
default `documents-anydoc` feature supports text and CSV-to-Markdown conversion;
minimal builds can disable that feature while retaining the contract and text
path. No parser receives a filesystem path, executes a helper, follows links,
or persists secrets.
