# Agent Plugins

Xana supports the declarative package boundary in the Agent Plugins 1.0.0
Working Draft. A plugin may contribute Agent Skills and MCP server declarations;
it cannot load native code, Rust extensions, hooks, scripts, or WASM into Xana.

Installation and enablement are separate. Inspecting or installing a local
package does not activate a skill, start a plugin-declared process, connect to a
plugin-declared endpoint, look up a credential, or change a profile. An
explicitly selected Git source does run bounded Git acquisition and HTTPS I/O.

## Review before installing

Review a local directory without copying it:

```text
xana plugin review PATH
```

Review an HTTPS Git source pinned to an exact 40-character commit:

```text
xana plugin review https://example.com/team/plugin.git --git --revision COMMIT
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
Portable package paths use bounded ASCII components so case, Unicode
normalization, Windows device aliases, and trailing-dot/space behavior cannot
materialize different trees across supported filesystems. Reinstalling the same source and digest is idempotent; changed content requires
the explicit update lifecycle rather than silently replacing an install.

Git is invoked only for an explicitly selected Git source. Xana fetches the
exact commit into a temporary bare repository with hooks and submodule
recursion disabled, validates the resulting archive, and records the exact
credential-free HTTPS URL and revision. User information, query strings, and
fragments are rejected. Git prompting, credential helpers, ambient Git
configuration, and URL rewrite rules are disabled for acquisition. Package
discovery never downloads schemas or other content.

## Linked development mode

Use a linked source only when actively developing a plugin:

```text
xana plugin review PATH --linked
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

## Inspect and enable exact scopes

Inspect installed health and lifecycle state, then enable only the intended
scope:

```text
xana plugin inspect quality-tools
xana plugin enable quality-tools
xana plugin enable quality-tools --project PROJECT_ID
xana plugin enable quality-tools --profile default
xana plugin enable quality-tools --project PROJECT_ID --profile review
```

The first command enables the user scope. `--project` selects one local project;
`--profile` selects a global profile; combining both selects one portable project
profile. Profile enablement adds the portable plugin name to that profile while
the exact installed revision and approval remain private local state. Resolution
freezes that exact revision into the conversation profile snapshot. A missing,
disabled, drifted, or invalid package is a readiness error rather than an
implicit install or fallback.

Enabled plugin skills join normal Agent Skills discovery under the qualified
name `plugin:PLUGIN/SKILL`. Cross-source same-name skills therefore remain
explicit rather than colliding silently. A plugin MCP declaration remains inert
during plugin lifecycle commands; it can start only through Xana's separate,
profile-allowlisted, outbound-guarded MCP runtime.

Disable the same exact scope with matching flags:

```text
xana plugin disable quality-tools --profile default
```

Disablement does not delete installed versions.

## Review updates and roll back

Updates are always two-step and exact:

```text
xana plugin update-check quality-tools
xana plugin update quality-tools --digest REVIEWED_DIGEST --yes
xana plugin update-check quality-tools --revision EXACT_GIT_COMMIT
xana plugin update quality-tools --revision EXACT_GIT_COMMIT --digest REVIEWED_DIGEST --yes
```

`update-check` reacquires and validates the candidate, shows added/removed skill
and MCP declarations, and records only its digest as pending. `update` reacquires
the source again and refuses any digest, source, or active-version race. If the
declared capability digest is unchanged, exact approved scopes carry forward.
Any broadened or changed capability starts disabled and requires explicit new
approval. Xana retains the prior immutable version as the rollback target:

```text
xana plugin rollback quality-tools --yes
```

Rollback atomically restores the prior version and only the scopes previously
approved for that exact revision. Linked development packages have no false
immutable rollback claim; source drift is reported as degraded until explicitly
reviewed and updated.

## Remove and collect unreachable data

```text
xana plugin remove quality-tools --yes
xana plugin gc --yes
```

Removal refuses enabled scopes, global or registered portable profile
references, and frozen conversation revisions with exact remediation. It never changes workspace content or
unrelated profiles. Garbage collection deletes only managed version directories
that no lifecycle record references. `xana doctor` reports installed package
health, active revision, scopes, and rollback availability without printing
manifest secrets or untrusted source text.
