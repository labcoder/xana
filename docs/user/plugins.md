# Agent Plugins

Xana supports the declarative package boundary in the Agent Plugins 1.0.0
Working Draft. A plugin may contribute Agent Skills and MCP server declarations;
it cannot load native code, Rust extensions, hooks, scripts, or WASM into Xana.

Installation and enablement are separate. Inspecting or installing a package
does not activate a skill, start a process, connect to an endpoint, look up a
credential, or change a profile.

## Inspect before installing

Review a local directory without copying it:

```text
xana plugin inspect PATH
```

Review an HTTPS Git source pinned to an exact 40-character commit:

```text
xana plugin inspect https://example.com/team/plugin.git --git --revision COMMIT
```

The review shows the manifest identity, content digest, compatible skills,
requested MCP process tokens or network destinations, and precise diagnostics
for ignored or invalid components. Environment and header values are never
printed. Unknown top-level `plugin.json` fields and a non-object `extensions`
field are reported and ignored as required by the standard; other manifest
schema violations reject the package. An invalid skill or MCP server is skipped
at its standard-defined failure boundary.

## Install disabled

`install` is review-only unless `--yes` is present:

```text
xana plugin install PATH
xana plugin install PATH --yes
xana plugin install https://example.com/team/plugin.git --git --revision COMMIT --yes
xana plugin list
```

Before committing, Xana reacquires the source and requires its digest to match
the displayed review. Normal installs copy a bounded, validated tree into a
private content-addressed store. Symlinks, junctions/reparse points, traversal,
special files, non-portable paths, submodules, oversized files/trees, revision
drift, and changes during copying fail before package state is published.
Reinstalling the same source and digest is idempotent; changed content requires
the explicit update lifecycle rather than silently replacing an install.

Git is invoked only for an explicitly selected Git source. Xana fetches the
exact commit into a temporary bare repository with hooks and submodule
recursion disabled, validates the resulting archive, and records the exact
revision. Package discovery never downloads schemas or other content.

## Linked development mode

Use a linked source only when actively developing a plugin:

```text
xana plugin inspect PATH --linked
xana plugin install PATH --linked --yes
```

Linked mode stores the canonical local path rather than a managed copy and is
always labeled mutable. Later source drift invalidates the prior review. It
cannot masquerade as an immutable install and remains unsuitable for unattended
execution.

## Storage and recovery

Package metadata is private Xana state in `data/interoperable/packages.json`.
Immutable trees live below `data/interoperable/plugins/versions/`; temporary
acquisition uses `.staging/`. None of these paths are portable project
configuration. An interrupted operation can leave an unreferenced staged or
content-addressed tree, but it cannot publish a partial installed record;
subsequent recovery/garbage collection can remove those unreachable files.

Package lifecycle operations use a cross-process lock plus atomic versioned
state replacement. A concurrent process receives a typed busy error and can
retry. Credentials, private environment values, and raw hostile control
sequences are excluded from package records and terminal diagnostics.

Enable, update, rollback, removal, and MCP process ownership are documented
separately as those lifecycle surfaces become available. Until a package is
explicitly enabled for a compatible scope, its contributions remain inert.
