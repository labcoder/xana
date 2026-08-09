# Full-screen terminal UI

> Audience: People using Xana in an interactive terminal.

Bare `xana` opens the full-screen Ratatui client when stdin and stdout are
terminals. Use `xana --tui` to require it or `xana --plain` to keep the
append-only interface. From a source checkout, put arguments after Cargo's
separator: `cargo run -- --tui`.

The TUI is a client of Xana's bounded frontend protocol. It cannot bypass the
workspace root gate, runtime permissions, model capabilities, or execution
owner. The current full-screen conversation path is native. Managed Codex
continues to use the plain terminal until its typed activity and approval
projection is attached.

## Composer and portable keys

The composer is multiline, UTF-8 aware, and bounded to 1 MiB. Arrow, Home, and
End keys move the cursor; Shift with those keys extends a selection. Backspace
and Delete edit the draft. Draft text and staged images are not added to model
context until a turn is accepted.

Two machine-local presets decide the unmodified Enter key:

| Preset | Enter | Ctrl+J | Modified Enter |
|---|---|---|---|
| `submit` | Submit | Newline | Shift+Enter newline; Ctrl+Enter submit |
| `newline` | Newline | Submit | Shift+Enter newline; Ctrl+Enter submit |

Ctrl+J is the portable alternate for terminals that do not distinguish
modified Enter. `/composer submit` and `/composer newline` change and persist
the preset in `data/frontend/presentation.toml`. The command palette always
offers Send.

Bracketed paste opens a confirmation preview. Xana removes terminal control
characters, normalizes line endings and tabs, and bounds the result before it
can enter the draft. A pasted `/command` remains untrusted text and is never
executed by the paste event. Enter confirms the preview; Esc discards it.

## Turns, follow-ups, and cancellation

Submitting while no root is active acquires the canonical workspace gate and
starts one correlated operation. While it is busy, ordinary submissions enter
a visible FIFO follow-up queue (at most 32 entries and 2 MiB including image
references). `/queue` shows the queue, `/queue edit N` returns one item to the
composer, and `/queue remove N` removes one item. A follow-up starts only after
the preceding root reaches a terminal state and releases its lease.

Ctrl+C or `/interrupt` requests interruption of the exact active operation;
Ctrl+Q or `/quit` shuts down the foreground client. Interruption and steering
are different commands. Native execution does not support same-turn steering,
so `/steer MESSAGE` reports that limitation instead of approximating it with a
queued message. Managed steering will be offered only when the active
app-server contract advertises it.

## Commands and pickers

Ctrl+P opens the searchable command palette. Palette actions and slash input
use one typed registry:

- `/help`, `/send [MESSAGE]`, `/newline`, `/quit`
- `/interrupt`, `/steer MESSAGE`
- `/model [CONNECTION/MODEL]`, `/reasoning [EFFORT]`
- `/activity quiet|normal|verbose`
- `/attach WORKSPACE_RELATIVE_PATH`, `/queue [edit|remove N]`
- `/clear`, `/composer submit|newline`

Up/Down changes the selected palette or picker item, Enter activates it, and
Esc closes the overlay. `/model` without an argument opens choices from the
configured and cached catalogs. A model change is persisted and starts a new
conversation; Xana does not translate history between execution owners or
models. Native reasoning control is unavailable. Activity visibility changes
only what the frontend renders and never changes model reasoning effort.

`/attach` accepts a workspace-relative regular image through Xana's existing
bounded artifact ingestion. It refuses traversal, symlink escape, unsupported
formats, oversized images, more than eight images, more than 20 MiB per turn,
or a selected model not declared image-capable.

## Layout and accessibility

Wide terminals show session, conversation, and activity columns. Medium and
narrow terminals prioritize conversation and composer content; overlays remain
keyboard accessible. The mouse wheel is an optional scrolling convenience and
grants no extra authority. Color is never the only state indicator. Theme,
Unicode/ASCII, reduced-motion, density, and composer preferences have safe
fallbacks documented in [Terminal presentation](presentation.md).

The current P5-07 client fails closed if an interactive approval reaches the
TUI. Use `xana --plain` when a native turn may require an `ask` decision until
the correlated approval-card slice is shipped. One-shot mode also fails closed
by design. Terminal state is restored on normal exit, error, cancellation, and
panic unwind.
