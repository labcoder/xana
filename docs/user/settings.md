# Settings workspace and configuration editing

> Audience: People installing, configuring, or using Xana.

`xana settings` is the ordinary place to understand and change Xana after
initial setup. It presents global runtime defaults and machine-local terminal
preferences in one workspace without pretending they are one file. The same
typed settings interface powers the full-screen browser, scriptable `xana
config` commands, and `/settings` from chat.

Connections, credentials, model catalogs, profile lifecycle, projects,
permission-rule collections, Skills, Agent Plugins, MCP servers, external
agents, focused image/vision routes, and recovery remain focused managers.
Settings summarizes those domains and names the exact manager command; it does
not flatten them into unsafe key/value edits.

## Start here

```bash
# Open the full settings workspace.
xana settings

# Open or search directly.
xana settings --section appearance
xana settings --search retention

# Inspect settings without an interactive terminal.
xana config list
xana config explain permissions.default

# Preview and apply one exact scalar edit.
xana config set diagnostics.max_total_bytes 64MiB --dry-run
xana config set diagnostics.max_total_bytes 64MiB
```

When stdin or stdout is redirected, `xana settings` prints the same filtered
catalog instead of attempting to enter an alternate screen. Use `xana config
list --json` when a program needs a stable document.

## What the workspace shows

The browser has nine stable sections:

| Section | Ordinary contents |
|---|---|
| Overview | Configuration health, effective default conversation, and manager summary |
| Appearance | Theme, glyphs, motion, density, Enter behavior, and activity-pane preference |
| Connections & Models | Read-only summaries with links to connection and model managers |
| Profiles & Routes | Global default profile plus links to profile and route managers |
| Permissions | Global fallback decision plus the focused rule builder |
| Execution | Command shell, optional shell executable, and default child route |
| Diagnostics | Logging, level, retention, file/total storage, file count, and queue capacity |
| Integrations | Agent Plugins, MCP, external agents, and focused service routes |
| Advanced | Durable paths and validated edit/migration entry points |

Every row names four facts that are easy to confuse in a raw configuration
file:

- **Current** is the effective value Xana validated.
- **Source** distinguishes a built-in default, `config.toml`, machine-local
  presentation preferences, or derived status.
- **Scope** says whether the edit targets this terminal frontend, global Xana
  configuration, or a focused task manager.
- **Effect** says whether the value applies immediately, to new conversations,
  on Xana's next launch, or only through another manager.

No stored key, environment-secret value, or credential-store handle enters a
settings snapshot or JSON result. Connection summaries contain kind/count and
readiness-oriented facts only.

## Full-screen workflow

The settings browser is persistent: moving, searching, editing, and opening a
review do not tear down and repaint a sequence of prompts. Wide terminals show
a section rail, settings list, and details together. Medium terminals keep the
section rail and stack list/details. Narrow terminals use a section carousel
and stacked content. At 80x24 all primary controls remain visible; extremely
small terminals show one safe resize instruction.

| Key | Action |
|---|---|
| Up/Down or `J`/`K` | Move through settings or choice values |
| Left/Right, Tab/Shift+Tab, or `H`/`L` | Move between sections |
| Page Up/Page Down, Home/End | Move by page or to an edge |
| Enter | Edit the selected value or confirm the current overlay |
| `/` or Ctrl+F | Search keys, labels, descriptions, and displayed values across all sections |
| `R` | Stage the selected setting's documented default |
| `U` | Revert only the selected staged edit |
| `A` or Ctrl+S | Open the exact review; confirm again to apply |
| `D` | Review discarding every staged edit |
| `?` or F1 | Open the keyboard map |
| Esc | Go back exactly one level |
| Ctrl+Q | Request exit; staged work still requires discard confirmation |

Choice/Boolean rows open a constrained picker. Numeric, byte-size, duration,
and optional-path rows open a bounded text editor with examples. Byte values
accept exact bytes or units such as `4MiB` and `32MB`; retention accepts values
such as `7`, `30d`, and `90 days`. Xana rejects blank, oversized, control-
character, unknown, out-of-range, or mutually incompatible values without
closing the editor or losing the current draft.

Appearance edits preview inside the settings workspace. ASCII glyph mode uses
ASCII borders, markers, shortcuts, and cursor hints; monochrome mode removes
color dependence. Reduced motion does not replace state or progress cues.

Staged rows receive a visible marker and are not durable yet. Apply first opens
a review containing every before/after value, target scope, and application
timing. Esc keeps editing. Discard says explicitly that durable files were not
changed. A failed validation or concurrent-edit check leaves the workspace
open with an actionable error.

## Scriptable inspection

```bash
xana config list [--section SECTION] [--search QUERY] [--json]
xana config get KEY [--json]
xana config explain KEY [--json]
```

`list` groups human output by section. `get` prints only the canonical value
when one exists, which makes it suitable for small scripts; an automatic or
summary row prints its safe display value. `explain` prints the friendly label,
stable key, current/default values, source, scope, effect, type, allowed
choices, description, and focused-manager command.

JSON catalog output has a document `version`, a content-derived `revision`,
warnings, and secret-free setting records. Setting and receipt enums use stable
snake-case identifiers. New consumers should ignore unknown fields and render
unknown setting kinds read-only rather than guessing a control.

## Scriptable mutation

```bash
xana config set KEY VALUE [--dry-run] [--json]
xana config reset KEY [--dry-run] [--json]
```

`set` and `reset` stage one edit through the same draft used by the TUI, run
complete record validation, and then commit. `--dry-run` returns the exact
redacted receipt without writing a config, backup, or presentation record.
Setting an already-effective value succeeds with an empty receipt.

Examples:

```bash
xana config set appearance.theme dark
xana config reset appearance.theme
xana config set permissions.default deny --dry-run
xana config set execution.shell powershell
xana config reset execution.shell_program
xana config set diagnostics.retention_days 30d
xana config set diagnostics.max_total_bytes "64 MiB" --json
xana config set notifications.completions false
xana config set notifications.enabled false --dry-run
```

Notification category settings take effect on the next graphical launch. They
control only fixed redacted attention hints while Xana is unfocused or
minimized; Activity and Diagnostics remain the authoritative state.

An attempt to mutate a summary row fails and prints its exact focused manager,
for example `xana connection list`. This is deliberate: connection removal,
credential changes, profile edits, and permission rules have coupled invariants
that a scalar settings interface must not erase.

## Safety and durable ownership

The settings module reads bounded configuration and presentation records and
validates the complete effective state before exposing a draft. A draft is
based on a content revision. At apply time Xana:

1. renders and validates the complete proposed records;
2. acquires the existing cross-process configuration transaction lock;
3. rejects a changed `config.toml` or presentation record instead of
   overwriting it;
4. retains the exact prior config as `config.toml.bak`;
5. installs each changed owner through atomic file replacement; and
6. restores both prior owners if a later write in the coordinated transaction
   fails.

Cancel and discard are byte-identical because they perform no write. The
human-authored `config.toml` and machine-local
`data/frontend/presentation.toml` remain separate versioned owners even though
one interface presents them together.

## Entering settings from chat

Use `/settings` or `/settings SECTION` while a native or managed conversation
is idle. Xana stops the foreground execution owner and restores the terminal
before entering settings. On return:

- presentation-only changes resume the same conversation with refreshed
  presentation;
- a setting marked **Applies to new conversations** starts a new conversation
  rather than mutating the frozen runtime snapshot; and
- cancellation resumes the originating plain or full-screen surface without
  changing durable state.

The TUI command palette contains the same `/settings [SECTION]` entry. Plain
chat advertises `/help`, which includes settings and its lifecycle behavior.

## Focused managers and advanced recovery

Use the linked manager when the job is richer than one scalar:

```bash
xana connect
xana connection list
xana model list
xana profile list
xana route list
xana plugin list
xana mcp list
xana external-agent list
xana image list
xana doctor
```

`xana config edit` remains the advanced escape hatch. It edits a bounded
temporary copy, validates the complete configuration, and installs it
atomically with a backup. `xana config migrate` previews schema/private-state
migration and requires `--apply` to mutate. `xana reset` removes explicit state
scopes and is not the same operation as `xana config reset KEY`.

If settings reports a concurrent edit, close/reopen the workspace or rerun the
CLI command after reviewing the other writer. If presentation preferences are
invalid, the workspace warns and displays safe defaults; applying an appearance
edit replaces that record with a valid current-version document. An invalid
global configuration must be repaired through setup, migration, validated
edit, or Doctor before settings can begin.
