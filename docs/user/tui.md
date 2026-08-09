# Full-screen terminal UI

> Audience: People using Xana in an interactive terminal.

Bare `xana` opens the full-screen Ratatui client when stdin and stdout are
terminals. Use `xana --tui` to require it or `xana --plain` to keep the
append-only interface. From a source checkout, put arguments after Cargo's
separator: `cargo run -- --tui`.

The TUI is a client of Xana's bounded frontend protocol. It cannot bypass the
workspace root gate, runtime permissions, model capabilities, or execution
owner. Native execution and managed Codex use the same state/update/view
shell. Codex still owns its thread, inner loop, tools, sandbox, and history;
Xana projects only the activity and approval callbacks app-server emits.

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
- `/activity auto|open|hidden`
- `/attach WORKSPACE_RELATIVE_PATH`, `/queue [edit|remove N]`
- `/clear`, `/composer submit|newline`
- `/sessions [expanded|collapsed]`

Up/Down changes the selected palette or picker item, Enter activates it, and
Esc closes the overlay. `/model` without an argument opens choices from the
configured and cached catalogs. A native model change is persisted and starts
a new conversation; Xana does not translate history between execution owners
or models. A managed Codex model or reasoning change applies to subsequent
turns and preserves the Codex thread. Native reasoning control is unavailable.
Activity visibility changes only what the frontend renders and never changes
model reasoning effort.

`/attach` accepts a workspace-relative regular image through Xana's existing
bounded artifact ingestion. It refuses traversal, symlink escape, unsupported
formats, oversized images, more than eight images, more than 20 MiB per turn,
or a selected model not declared image-capable.

Assistant Markdown, code, diffs, tables, inert links, images, and immutable
artifacts use a bounded terminal-native renderer. `/artifact ARTIFACT_ID`
opens an explicit action card; rendering alone never opens a link, file, or OS
application. See [Rich terminal content and artifacts](rich-content.md).

## Conversation navigation

`/sessions` opens a searchable, keyboard-complete picker backed by the bounded
workspace-host snapshot. On wide terminals `/sessions expanded` and
`/sessions collapsed` persist the default rail state for this workspace.
Selecting another native conversation opens its committed history for
inspection while leaving an active root attached to its original conversation;
managed history remains owned by Codex. Drafting remains local, but Xana will
not submit a draft from a read-only historical view. Return to the runtime
conversation or use the exact resume command shown in [Sessions](sessions.md).
Native history initially loads at most 128 messages; scrolling to the older
edge requests another bounded page while preserving the current anchor.

## Layout and accessibility

Wide terminals show session, conversation, and activity columns. Medium and
narrow terminals prioritize conversation and composer content and show
activity as a drawer. `auto` opens for substantive plans, tools, children,
commands, diffs, managed work, warnings, errors, or approvals and collapses
after the next submitted message. `open` pins it; `hidden` keeps a compact
status, but cannot conceal an approval or critical failure. The explicit mode
is persisted in `data/frontend/presentation.toml`.

Activity cards retain execution ownership: Xana roots, Xana children, Codex
managed turns, and Codex-owned collaboration are not merged. Reasoning
summaries and raw reasoning are labeled separately from assistant messages and
appear only when the execution owner emits them. No display action asks a
model to summarize, changes reasoning effort, or adds tokens.

Approval cards show the requesting owner, tool or Codex callback, final
arguments, and scope. Enter chooses the highlighted allow-once, exact-session,
or deny action; Esc leaves the correlated request pending. A decision is sent
exactly once through the existing permission/runtime callback. Approval cards
appear even when activity is hidden. One-shot mode continues to fail closed by
design.

The mouse wheel is an optional scrolling convenience and grants no extra
authority. Color is never the only state indicator. Theme, Unicode/ASCII,
reduced-motion, density, and composer preferences have safe fallbacks
documented in [Terminal presentation](presentation.md). Terminal state is
restored on normal exit, error, cancellation, and panic unwind. Closing the
embedded managed TUI cancels its correlated active Codex turn, resolves any
outstanding approval closed, and shuts down app-server.
