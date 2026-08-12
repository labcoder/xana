# Agent Skills

Xana supports the required `SKILL.md` contract from the Agent Skills
specification snapshot `69ef37e9424c0a7ea9dd2293b559e43ec8176379`.
Skills are portable instruction packages, not executable Xana extensions and
not authority grants.

## Discovery scopes

Xana discovers direct child skill directories from these bounded roots:

1. user scope: `~/.agents/skills/`;
2. project scope: `<workspace>/.agents/skills/`; and
3. the `skills/` directory of an explicitly enabled Agent Plugin.

Each skill directory must contain `SKILL.md`. `xana skill list` reads only its
bounded YAML frontmatter, so inactive bodies and resources do not enter model
context or materially increase startup work. Use `--workspace PATH` to inspect
a different project scope.

```text
xana skill list
xana skill inspect project/review
xana skill validate .agents/skills/review
```

The standards fields `name`, `description`, `license`, `compatibility`,
`metadata`, and experimental `allowed-tools` are recognized. Xana deliberately
does not treat `allowed-tools` as pre-approval: only the resolved profile,
capability registry, permission broker, and egress policy can grant runtime
authority.

## Qualification and activation

Qualified identities are `user/NAME`, `project/NAME`, and
`plugin:PLUGIN/NAME`. An unqualified name works only when exactly one discovered
source provides it. Collisions fail and list every qualified choice; Xana never
silently chooses by precedence.

```text
xana skill activate project/review
xana skill read project/review references/CHECKLIST.md
xana skill enable project/review --profile default
xana skill disable project/review --profile default
```

`activate` performs a bounded full read and validates directly referenced
Markdown resources. `enable` records the qualified reference in the selected
global profile; add `--project PROJECT_ID` for a portable project profile.
Profile changes apply to new/future resolved prompt snapshots. Existing frozen
conversation snapshots remain immutable.

Activated content appears in the system prompt with exact path, qualified
source, and digest provenance. It remains untrusted guidance below Xana's
non-replaceable identity/guidelines and typed authority. Activation never runs a
script, starts a process, reads credentials, installs a package, or enables an
MCP server.

## Safety and limits

- `SKILL.md` is a non-symlink regular UTF-8 file capped at 256 KiB; frontmatter
  is capped at 32 KiB.
- Resource paths must be relative, contained under the skill root, and contain
  no symlink component.
- Automatic activation loads only Markdown links under `references/`, with 32
  files, 64 KiB per file, 256 KiB total, and four reference levels.
- Cycles, traversal, special files, invalid UTF-8, size overflow, and a file
  changing during activation fail closed.
- `scripts/` and `assets/` are never executed or automatically loaded. A later
  explicit tool action remains subject to normal capabilities and permissions.

Plain chat and the TUI expose the same typed operations as `/skill ...`. The
TUI temporarily restores ordinary terminal mode to render the command result,
then reopens the conversation surface.
