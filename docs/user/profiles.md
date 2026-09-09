# Profiles

> Audience: People creating reusable Xana behavior and authority presets.

A profile is a named, versioned selection of connection, model, reasoning,
guidance, capabilities, integrations, permissions, and budget ceilings. Xana
ships no fixed role names: create names that fit your work. Profiles do not
inherit from one another; `duplicate` makes an independent copy with a new
stable identity.

## User-global profiles

```bash
xana profile list
xana profile create personal
xana profile create work --make-default
xana profile create review --connection ollama --model qwen3:8b
xana profile default personal
xana profile inspect review
xana profile edit review --identity "Review carefully." --max-tool-rounds 2
xana profile duplicate review fast-review
xana profile rename fast-review quick-review
xana profile archive quick-review
xana profile list --all
xana profile unarchive quick-review
xana profile delete quick-review --yes
```

Creating a global profile copies the current default's concrete settings into a
new identity. Omit connection and model to reuse both; no new account, API key,
model download, or provider call is needed. Override the model alone to use a
different model on that connection. Changing connection requires its model too.
Integration requirements are copied, but private scoped enablement is not
granted: inspect `profile resolve NAME` for readiness.

Default is a designation, not a reserved name. Full Setup names the first
profile before appearance and creates exactly that profile as default. Flags
can name it with `setup --profile xana-dev`. Later additions leave the default
unchanged unless you select **Make default**, pass `--make-default`, or run
`profile default NAME`. Existing conversations keep their identity.

At least one active, primary-capable global profile must remain. Archived and
child-only profiles do not satisfy that rule; temporary provider outages do not
change it. You can delete or archive the initial profile, including one named
`default`, once an eligible replacement exists:

```bash
xana profile delete personal
xana profile delete personal --replacement work --yes
# Archive uses the same review when the default or child routes are affected:
xana profile archive work
```

The preview suggests the next eligible profile in the name-sorted list, wrapping
at the end. Desktop lets you cycle replacement candidates before confirming.
CLI automation must name `--replacement` when removing the default with several
eligible successors; a sole successor can be inferred. Configuration changes
invalidate an open removal review. Removing dependent child routes is listed in
the review: they are removed, never silently reassigned; unarchiving does not
recreate them. Registered project authority references must be explicitly
updated before their global profile can be renamed or removed.

The profile UUID remains stable across edits and rename, including legacy
profiles without an explicit UUID. Duplication and creation receive new UUIDs.
Removal keeps credentials, conversations, artifacts and profile-private memory;
it neither transfers that memory to the successor nor deletes its contents.
Scheduled work retains its original binding and must be reviewed if that binding
no longer resolves; a new default does not reroute it.

## Project-local profiles

Project profiles live in the explicitly shared `.agents/xana/project.toml` and
must name a user-global authority profile. They use logical connection names;
private local bindings stay outside the workspace.

```bash
xana project share PROJECT_ID
xana profile create safe-review --project PROJECT_ID \
  --authority-profile review --connection chat --model qwen3:8b
xana project register --workspace PATH
xana project bind PROJECT_ID chat ollama
xana project refresh PROJECT_ID
xana profile resolve safe-review --project PROJECT_ID
```

Use `--project PROJECT_ID` with list, inspect, edit, duplicate, rename, archive,
unarchive, delete, and resolve. Project edits preserve unrelated manifest
content. A project profile may narrow the named global ceiling but cannot add a
capability, integration, outbound-data class, permission, or budget outside it.
The rejected diagnostic names the exact field.
Project profiles are optional: deleting the last one leaves the collection
empty. Removing a project default selects the next eligible project profile, or
clears that optional pointer when none remains; it never substitutes a different
global authority ceiling.

## Resolution and readiness

```bash
xana profile resolve review
xana profile resolve review --json
```

Resolution is pure, bounded, deterministic, and network-independent. The
output includes every effective value and its provenance. Readiness is separate:
missing bindings or disabled/missing integrations produce exact setup reasons
without changing the resolved profile. Output contains references and redacted
metadata only—never API keys, OAuth tokens, credential values, or provider
thread handles.

Plugin names in a profile are portable logical requirements. Local package
installation and scoped enablement resolve each name to one reviewed content
digest; that exact `plugin_revisions` map is included in the resolved/frozen
snapshot. Xana never substitutes another version silently. Use `xana plugin
enable NAME --profile PROFILE` (and add `--project PROJECT_ID` for a project
profile) to update the portable reference and private binding together.

## Conversation snapshots

```bash
xana profile freeze review CONVERSATION_ID
xana profile continue safe-review CONVERSATION_ID --project PROJECT_ID
```

Starting work freezes one immutable resolved profile snapshot for that
conversation. Freezing the same result again is idempotent; trying to replace it
fails. Selecting another profile creates a new linked continuation and preserves
the source conversation, owner-specific model/reasoning history, and frozen
snapshot. The current commands expose this durable contract; interactive
project/profile placement uses the same domain operations.

Native conversation resolution follows the saved UUID after a rename, not the
old name or the new default. Reusing a deleted name cannot claim the old
conversation or its memory. A retired native profile leaves history viewable,
but a new turn requires restoring the archived profile or explicitly choosing a
linked continuation. Managed Codex retains its existing frozen authority and
vendor-owned conversation lifecycle; changing a default is not a Codex handoff.

Profile identity text and instructions are guidance. They do not grant tool
authority, change permission policy, or make repository/model/tool content
trusted.
