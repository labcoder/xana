# Projects

> Audience: People organizing Xana conversations across workspaces.

Xana projects are optional local organization. A project has a stable UUID,
display name, and one canonical workspace directory. It does not own that
directory: create, rename, archive, relink, and forget do not write or delete
workspace files, sessions, artifacts, credentials, or provider history.
Conversations that are not assigned to a project remain **Ungrouped**.

## Create and inspect

```bash
xana project create "My project"                 # current directory
xana project create "My project" --workspace PATH
xana project list
xana project list --all                           # include archived
xana project inspect PROJECT_ID
```

Only one project may use a canonical workspace. Symlink aliases resolve to the
same identity. A missing workspace is reported read-only; bind an intentionally
moved project with `xana project relink PROJECT_ID PATH`. Relink refuses a path
already owned by another project.

## Lifecycle

```bash
xana project rename PROJECT_ID "New name"
xana project archive PROJECT_ID
xana project unarchive PROJECT_ID
xana project forget PROJECT_ID --yes
```

Archive only removes a project from the default active list. Forget removes the
local registry entry and returns its conversations to Ungrouped. Both preserve
the workspace and all conversation/provider history. Forget requires `--yes`
because the local organizational metadata itself is intentionally removed.

## Conversation placement

```bash
xana project assign PROJECT_ID CONVERSATION_ID
xana project membership CONVERSATION_ID
xana project ungroup CONVERSATION_ID
```

Assignment is allowed only when the conversation and project use the same
canonical workspace; it changes a separate membership relation rather than
rewriting immutable session history. Cross-workspace movement requires a new
conversation. Review the owner-specific placement with:

```bash
xana project continue PROJECT_ID CONVERSATION_ID --owner native
xana project continue PROJECT_ID CONVERSATION_ID --owner codex
```

The command is read-only unless `--apply` is present. It states whether the
existing conversation can be reassigned or a fresh native/Codex-owned
conversation must start, and it never silently copies transcript text. Select a
profile explicitly with `--profile NAME`; otherwise the project default or
user-global default resolves deterministically.

```bash
xana project continue PROJECT_ID CONVERSATION_ID --owner native --profile review
xana project continue PROJECT_ID CONVERSATION_ID --owner native --profile review --apply
```

An applied same-workspace continuation without a profile change assigns the
existing conversation atomically. A cross-workspace or profile-changing native
continuation creates an empty resumable target session, records project,
predecessor, and frozen-profile provenance together, and preserves the source.
A managed Codex continuation records a pending target with a frozen Codex
profile; `xana --resume TARGET_ID` in the target workspace starts a fresh vendor
thread on its first turn. Xana never translates provider history between owners.

From plain chat or the TUI, `/project ...` and `/profile ...` restore ordinary
terminal mode, invoke these same typed operations, then return to the prior chat
surface. Quoted arguments are bounded and parsed consistently. The command
palette exposes both families for keyboard-only discovery.

## Portable project configuration

Private projects remain the default. Sharing is an explicit effect:

```bash
xana project share PROJECT_ID
xana project inspect-portable --workspace PATH   # pure read-only review
xana project register --workspace PATH           # local registration only
xana project diff PROJECT_ID
xana project refresh PROJECT_ID                  # accept reviewed manifest digest
xana project setup PROJECT_ID                    # redacted readiness
xana project bind PROJECT_ID LOGICAL LOCAL_NAME  # private local mapping
xana project stop-sharing PROJECT_ID --yes
```

`share` creates only `.agents/xana/project.toml`. This is a Xana-specific
portable file, not part of the Agent Skills or Agent Plugins standards. It may
declare a portable identity, safe project profiles, skill/plugin/MCP/external-
agent references, focused-service requirements, and narrowing defaults. It may
not contain credentials, endpoints, absolute local paths, session or provider
thread IDs, permission grants, personal preferences, or private registry data.

Repository manifests are untrusted policy. Inspection is bounded metadata I/O
only: it does not access credentials, contact a network, start a process,
install or enable a package, authenticate an endpoint, or change a conversation.
Project profiles must name a user-global authority profile and may only narrow
its permissions, capabilities, budgets, integrations, and outbound-data class
ceiling. A broadening field fails with its exact name.

Registration stores the canonical local path, reviewed manifest digest, and
logical-to-local bindings in Xana's private versioned record. Portable logical
names never reveal the local provider/service name. Missing or invalid local
bindings leave the project registered but not ready; `project setup` prints an
exact `project bind` action with binding values redacted. Editing the manifest
makes the registration stale until `project refresh` accepts the reviewed
digest. Stop-sharing removes only the manifest and preserves local project,
bindings, workspace, and history.

## Private state

Project identity and membership live in Xana's versioned private data record,
not the workspace. Run `xana config migrate --apply` if the record has not been
initialized. Portable `.agents/xana/project.toml` sharing is a separate,
explicit M3-04 operation; private project creation never creates it.
