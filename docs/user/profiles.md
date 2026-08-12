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
xana profile create review --connection ollama --model qwen3:8b
xana profile inspect review
xana profile edit review --identity "Review carefully." --max-tool-rounds 2
xana profile duplicate review fast-review
xana profile rename fast-review quick-review
xana profile archive quick-review
xana profile list --all
xana profile unarchive quick-review
xana profile delete quick-review --yes
```

The profile UUID remains stable across edits and rename. A duplicate receives a
new UUID and has no inheritance link. The default profile and profiles used by
routes cannot be archived or deleted.

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

Profile identity text and instructions are guidance. They do not grant tool
authority, change permission policy, or make repository/model/tool content
trusted.
