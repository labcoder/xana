# Espejo

> Audience: People supervising local Xana work in the full-screen terminal UI.

Espejo is Xana's bounded work-and-attention perspective. Open it from the TUI
with `/espejo` or `/espejo global`; use `/espejo project` to restrict the view to
the selected Conversation's Project. `Ungrouped` is a real Project scope.

The word **global** currently means every retained Conversation in the current
local workspace snapshot. It does not imply another machine, a remote service,
or an account-wide scheduler. Remote supervision is outside Milestone 4.

## What the view shows

The summary separates `Needs you`, `In motion`, `Blocked`, `Failed`, and `Idle`
with text and markers so color is never the only signal. It also reports recent
completed Activity, the workspace-host state, the active root process when one
is known, and a workspace collision when another Conversation owns that root.

The Work list is capped at 512 projected Conversations. Its selected Evidence
pane identifies the Conversation, Project, execution owner, connection, model,
retained record count, current Run, queued input, waiting approvals, observed
tool/child/artifact Activity, and available usage facts. Missing optional facts
stay explicit: if the current adapter has no measured performance or semantic
completion receipt, Espejo says `unavailable` rather than deriving one from a
spinner or completed Activity. Xana does not render an empty `Coming up`
section before scheduled work exists.

Espejo is a projection, not an execution owner. Enter previews the selected
Conversation and returns to the conversation screen; use the Conversation
picker or `/conversation attach ID` for an ownership transition. Previewing one
item acknowledges only the relevant viewed/error marker, never all attention.

## Navigation

| Input | Result |
|---|---|
| Up/Down or mouse wheel | Move through the bounded Work list; the selected row remains visible. |
| Click a visible Work row | Select that same row. |
| Enter | Preview the selected Conversation and return to Conversation. |
| `G` | Show the global current-workspace scope. |
| `P` | Show the selected Conversation's Project or `Ungrouped`. |
| `A` | Open the newest detailed Activity card when one exists. |
| `D` | Run the existing redacted Diagnostics/doctor flow. |
| Ctrl+P | Open the shared command palette. |
| Esc | Return to the Conversation screen. |

At wide sizes, Work and Evidence appear side by side. Medium and narrow layouts
stack them. Below 42 columns by 12 rows, the view fails softly with an exact
resize instruction. ASCII/no-color and reduced-motion preferences preserve the
same words and actions; Espejo itself has no decorative animation. Plain mode
remains the screen-reader and automation-safe fallback.

See [Full-screen terminal UI](tui.md), [Conversations](sessions.md), and
[Commands and capability discovery](commands.md) for the surrounding contracts.
