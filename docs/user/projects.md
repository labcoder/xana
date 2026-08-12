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

The review is deliberately non-executing in M3-03. It states whether the
existing conversation can be reassigned or a fresh native/Codex-owned
conversation must start, and it never silently copies transcript text. The
interactive execution and project/profile navigation surface land together in
M3-06 so CLI, plain mode, and the TUI share one command path.

## Private state

Project identity and membership live in Xana's versioned private data record,
not the workspace. Run `xana config migrate --apply` if the record has not been
initialized. Portable `.agents/xana/project.toml` sharing is a separate,
explicit M3-04 operation; private project creation never creates it.
