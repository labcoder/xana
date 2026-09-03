# Espejo

> Audience: People supervising local Xana work in the full-screen terminal UI or native Desktop.

Espejo is Xana's bounded work-and-attention perspective. In Desktop, choose
**Espejo** in the fixed sidebar, the View menu, or the command palette. In the
TUI, use `/espejo` or `/espejo global`; `/espejo project` restricts the view to
the selected Conversation's Project. `Ungrouped` remains first-class rather
than being assigned to an invented Project.

In Desktop, **global** means the bounded set of Conversations registered with
the current local application host across its workspaces. In the TUI it means
the current local workspace snapshot. Neither meaning implies another machine,
a remote service, or an account-wide scheduler. Remote supervision is outside
Milestone 4.

## What the view shows

The summary separates `Needs you`, `In motion`, `Blocked`, `Failed`, and `Idle`
with text and markers so color is never the only signal. It also reports recent
completed Activity, the workspace-host state, the active root process when one
is known, and a workspace collision when another Conversation owns that root.

The terminal Work list is capped at 512 projected Conversations. Desktop uses
the host's bounded navigation and execution snapshots and groups cards under
**Needs you**, **In motion**, **Blocked or failed**, **Recently completed**, and
**Idle**. A card identifies Project/Ungrouped placement, workspace, execution
owner, connection/model/Profile, Run or last outcome, permission mode,
controller state, queued input, approvals, and observed Activity count. Open the
exact Conversation for its detailed Activity, artifacts, usage, and completion
receipt; Espejo does not fabricate those facts when the global host projection
does not carry them.

Missing optional facts stay explicit. If an adapter has no measured
performance, semantic receipt, or observer fact, Espejo says it is unavailable
rather than deriving one from a spinner. Xana does not render an empty
`Coming up` section before scheduled work exists.

Espejo is a projection, not an execution owner. In Desktop, opening an ordinary
card attaches the exact Conversation and returns to Conversation; a Needs-you
card routes to its Activity and approval controls. In the TUI, Enter previews
the selected Conversation and returns to Conversation; use the Conversation
picker or `/conversation attach ID` for an ownership transition. Navigation
does not grant controller authority or clear unrelated attention.

## Terminal navigation

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

## Desktop navigation

- Use the global and selected-Project scope buttons plus the state filters; only
  the card collection scrolls.
- Activate a card to open its exact Conversation. Needs-you cards open Activity
  so the runtime-issued approval or failure remains actionable.
- Redacted process-wide notices appear above the groups. **Open Diagnostics**
  opens the existing Diagnostics settings section; detailed Conversation errors
  stay in that Conversation's Activity.
- Native notifications focus the existing Desktop instance and route to the
  relevant Conversation, Activity, or redacted Espejo/Diagnostics path.
- **Back to Conversation** leaves host work untouched. Observers can inspect and
  navigate, while runtime commands continue to enforce controller authority.

At wide sizes, Work and Evidence appear side by side. Medium and narrow layouts
stack them. Below 42 columns by 12 rows, the view fails softly with an exact
resize instruction. ASCII/no-color and reduced-motion preferences preserve the
same words and actions; Espejo itself has no decorative animation. Plain mode
remains the screen-reader and automation-safe fallback.

See [Full-screen terminal UI](tui.md), [Conversations](sessions.md), and
[Commands and capability discovery](commands.md) for the surrounding contracts.
