# Full-screen terminal UI

> Audience: People using Xana in an interactive terminal.

Bare `xana` opens the full-screen Ratatui client when stdin and stdout are
terminals. Use `xana --tui` to require it or `xana --plain` to keep the
append-only interface. From a source checkout, put arguments after Cargo's
separator: `cargo run -- --tui`.

The TUI is a client of Xana's bounded frontend protocol. It cannot bypass the
workspace root gate, runtime permissions, model capabilities, or execution
owner. Native execution and managed Codex use the same state/update/view shell
and one bounded dirty-frame runner; input and execution events request a redraw
instead of drawing synchronously for every event. Codex still owns its thread,
inner loop, tools, sandbox, and history;
Xana projects only the activity and approval callbacks app-server emits.

## Composer and portable keys

The composer is multiline, UTF-8 aware, and bounded to 1 MiB. It grows from
one to six visible text rows, then scrolls internally to keep the cursor in
view without taking over the conversation. Arrow, Home, and End keys move the
cursor; Shift with those keys extends a composer selection. Backspace and
Delete edit the draft. Draft text and staged images are not added to model
context until a turn is accepted. The border states the active Enter behavior
instead of referring to an unnamed preset.

Submitted drafts are retained in a bounded, machine-local history for the
canonical workspace. Ctrl+Up and Ctrl+Down move through it; Up and Down do the
same when the composer is empty or already recalling history. Editing exits
recall, and moving past the newest item restores the draft that was present
before recall. Xana keeps at most 128 entries and 1 MiB, replaces secret-shaped
entries as a whole, and treats an unavailable or invalid history file as a
non-fatal frontend warning. This convenience history is not Conversation
authority and is never injected into model context on its own.

Type `@` followed by any part of a workspace-relative path and press Ctrl+Space
to open fuzzy file completion. Candidates come from the same bounded,
gitignore-aware workspace discovery used by native tools. The scan runs off the
render path, is cancelable and bounded to 32 visible results. Inserting a name
grants no read or disclosure authority; normal tool and outbound policy still
applies when Xana later uses the reference. Plain mode exposes the same
completion through Tab.

Two machine-local presets decide the unmodified Enter key:

| Preset | Enter | Ctrl+J | Modified Enter |
|---|---|---|---|
| `submit` | Submit | Newline | Shift+Enter newline; Ctrl+Enter submit |
| `newline` | Newline | Submit | Shift+Enter newline; Ctrl+Enter submit |

Ctrl+J is the portable alternate for terminals that do not distinguish
modified Enter. `/composer submit` and `/composer newline` change and persist
the preset in `data/frontend/presentation.toml`. The command palette always
offers Send.

Bracketed paste opens a confirmation preview. Some terminals deliver a paste
as a paced sequence of ordinary key events instead; Xana uses a short adaptive
quiet window to coalesce that bounded stream before interpreting Enter, so a
multiline paste cannot become several submitted messages or redraw one
character at a time. Xana removes terminal control characters, normalizes line
endings and tabs, and bounds the result before it can enter the draft. A pasted
`/command` remains untrusted text and is never executed by the paste event.
Enter confirms the preview; Esc discards it.

## Turns, follow-ups, and cancellation

Submitting while no root is active acquires the canonical workspace gate and
starts one correlated operation. While it is busy, ordinary submissions enter
a visible FIFO follow-up queue (at most 32 entries and 2 MiB including image
references). `/queue` shows the queue, `/queue edit N` returns one item to the
composer, and `/queue remove N` removes one item. A follow-up starts only after
the preceding root reaches a terminal state and releases its lease.

When a native turn consumes its configured soft round tranche, Xana preserves
the same root operation and displays its committed progress, cumulative usage,
remaining immutable ceiling, and any repeated exact tool-call-pattern count.
Use `/continue` to grant only the next configured tranche or `/stop` to finish
that exact operation as declined. Neither action creates a new user message;
`/continue` does not reset other budgets or prior effects. The workspace root
remains owned while the decision is pending, so queued follow-ups cannot jump
ahead. After restart, the same unresolved suspension is shown again.

Ctrl+C copies a retained conversation selection when one exists. Otherwise it,
or `/interrupt`, requests interruption of the exact active operation;
Ctrl+Q or `/quit` shuts down the foreground client. Interruption and steering
are different commands. Native execution does not support same-turn steering,
so `/steer MESSAGE` reports that limitation instead of approximating it with a
queued message. Managed steering will be offered only when the active
app-server contract advertises it.

## Commands and pickers

Ctrl+P opens the searchable command palette. It renders the typed registry as
a table with fixed Command, Mode or Parameters, and Description headings. The
selected row stays in view while Up/Down or the mouse wheel moves through the
scrollable body. Filtering matches command names, modes, parameters, and
descriptions; both `con` and `/ses` find the canonical `/conversation` family.

Palette actions and slash input use that one registry:

- `/help`, `/header view hide|show`, `/send [MESSAGE]`, `/newline`, `/quit`
- `/interrupt`, `/steer MESSAGE`, `/continue`, `/stop`
- `/model [CONNECTION/MODEL]`, `/reasoning [EFFORT]`
- `/activity view auto|hide|show`
- `/attach PATH|--clipboard|list|clear`, `/queue [edit|remove N]`
- `/clear`, `/compact`, `/composer submit|newline`
- `/conversation`, `/conversation new`, `/conversation continue`, `/conversation preview ID`,
  `/conversation attach ID`, `/conversation archive [ID]`,
  `/conversation search QUERY`, `/conversation view hide|show`
- `/project [SUBCOMMAND ...]`, `/profile [SUBCOMMAND ...]`, `/skill [SUBCOMMAND ...]`, `/plugin [SUBCOMMAND ...]`
- `/mcp [SUBCOMMAND ...]`, `/external-agent [SUBCOMMAND ...]`, `/image [SUBCOMMAND ...]`
- `/connection [SUBCOMMAND ...]`, `/connect [provider|profile|image|vision]`
- `/logs [path|list|show|export ...]`, `/outbound [list|revoke ...]`
- `/operation [plan|resume ...]`, `/route [list|check ...]`
- `/setup [quick|full|connection|permissions-shell|profiles-routes|appearance]`
- `/settings [overview|appearance|connections|profiles|permissions|execution|diagnostics|integrations|advanced]`
- `/usage [compact|details]`, `/capabilities`
- `/espejo [global|project]`
- `/doctor`

Up/Down changes the selected palette or picker item, Enter activates it, and
Esc closes the overlay. `/model` without an argument opens choices from the
configured and cached catalogs. A native model change is persisted and starts
a new conversation; Xana does not translate history between execution owners
or models. A managed Codex model or reasoning change applies to subsequent
turns and preserves the Codex thread. Native reasoning control is unavailable.
Activity visibility changes only what the frontend renders and never changes
model reasoning effort.

`/compact` is idle-only and applies only to native conversations whose context
Xana owns. It durably summarizes older entries into a bounded continuation and
keeps the recent tail verbatim; it never deletes raw session history. The
activity pane reports planning, start, source range/digest, model/budget facts,
completion, or an honest unavailable result. Managed Codex owns its context,
so the same command explains that limitation instead of approximating it.

`/mcp list` runs inside the attached TUI and presents its bounded output in a
scrollable result modal. Bare `/profile create` opens a three-field form with
the current connection and model prefilled, then creates the profile through
the same typed command/configuration transaction as the CLI. Errors remain in
the modal rather than tearing down and flashing the terminal. Other management
subcommands continue through the explicit foreground-owner transition. Xana
requires an idle Run, restores the terminal, invokes the exact typed top-level
CLI command (including its confirmations and secure input), and then returns
to the Conversation. The TUI does not parse configuration or credentials on a
separate path.

`/setup` restores the chat terminal before opening the same keyboard-driven,
full-screen selectors as `xana setup`. A focused section is available from
slash input or the searchable palette; configuration changes end the current
foreground owner explicitly and return only after the reviewed setup operation
completes. Repeated transitions use one application-owned restart loop rather
than nesting frontend launches.

`/settings [SECTION]` follows the same owner-safe lifecycle and opens the
persistent settings workspace described in [Settings workspace and
configuration editing](settings.md). It keeps a section rail/list/detail view
on wide terminals, adapts through 80x24 and narrow layouts, searches every
setting, previews appearance, and stages edits until a scope/effect review is
confirmed. Cancelling returns to the originating surface. Presentation-only
edits preserve the conversation; changes to frozen defaults start a new one.

`/usage` and `/usage compact` add a compact card to the conversation. They keep
current-Run facts, current-Conversation facts, and current-process accounting
visibly separate. `/usage details` opens a scrollable report containing every
available semantic observation, prompt-plan category, execution fact,
completion receipt, context-capacity fact, and surface capability. Native
request observations are deltas; managed Codex observations are cumulative
snapshots and replace an older observation for the same period instead of
being added again. Partial, estimated, stale, unsupported, and unavailable
values are labeled rather than treated as zero. Account quota, rate-limit
reset, and wallet/credit balance remain unavailable when the active connection
does not expose them through a supported interface. Neither command performs
an implicit account refresh.

`/doctor` pauses an idle foreground owner, restores the terminal, runs the
same redacted read-only report as `xana doctor`, then resumes the selected
conversation without translating history. Reset is intentionally not a slash
command. The searchable `Reset Xana state…` palette action is available only
while idle; it stops the owner, restores the terminal, previews an exact scope,
and requires the same filesystem and credential confirmations as the CLI.

`/attach PATH` accepts a bounded local regular file. `/attach --clipboard`
stages a supported clipboard image, `/attach list` reports the staged set, and
`/attach clear` removes that set without touching immutable artifacts already
published. Paths may identify PNG, JPEG, GIF, WebP, SVG, Lottie JSON, WAV, MP3,
Ogg, WebM, MP4, M4V, or an ordinary JSON document. Xana normalizes quoted,
`file://`, Windows, and Git Bash paths; rejects traversal, symlink escape,
non-regular files, inconsistent declared/detected types, and configured or
compiled-limit violations; then publishes one immutable artifact reference.

Workspace files use workspace authority. An existing file outside the
workspace always receives a single exact allow-once review before Xana reads
its bytes. Denial or Esc restores the draft without importing a partial set.
The review is acquisition authority only: it does not grant provider
disclosure, playback, transformation, or external-open authority.

Dragging or pasting a recognized local resource path into the TUI stages the
terminal-pasted path through that same ingestion path instead of inserting it
into the composer.
An ordinary message that contains one or more image-looking local paths is
recognized the same way, so `/attach` is a discoverable explicit action rather
than a requirement. Xana preserves path order, reviews all external paths in
one allow-once prompt, and does not send a partially attached turn if any path
is denied or invalid.
Current provider routes accept only validated PNG, JPEG, and GIF image inputs
when the exact model advertises image input. A non-image-capable model receives
ordinary message text unchanged, so it can explain the capability limit or
request a separately permissioned tool instead of Xana falsely claiming
vision. Other typed resources remain staged with an explicit metadata fallback
and cannot be submitted until an exact provider-input route exists; Xana
retains the draft and explains the missing capability rather than silently
disclosing bytes or pretending the model received them.

`appearance.inline_image = "auto"` is the default. It enables a terminal image
preview only after Xana positively identifies a supported protocol and usable
terminal dimensions; unproven multiplexers and failed probes use the metadata
fallback. Set it to `"off"` in `data/frontend/presentation.toml` or through
Settings to disable terminal image escape sequences unconditionally. This is a
presentation choice and does not change provider image-input capability.

Assistant Markdown, code, diffs, tables, inert links, images, and immutable
artifacts use a bounded terminal-native renderer. `/artifact ARTIFACT_ID`
opens an explicit action card; rendering alone never opens a link, file, or OS
application. See [Rich terminal content and artifacts](rich-content.md).

## Conversation navigation

`/conversation` opens a searchable, keyboard-complete picker backed by the bounded
workspace-host snapshot and displays each exact native or managed Conversation ID.
Enter attaches to an eligible idle Conversation; Space previews it without
changing the execution owner. `/conversation attach ID` performs the same exact
attach action without the picker. An active source Run, queued source input, a
target controlled by another process, an unavailable target, or an execution-owner
mismatch leaves the current attachment unchanged and reports the recovery action.
On wide terminals `/conversation view show` and `/conversation view hide` persist the
panel state for this workspace; clicking the visible panel title also hides
it. A hidden panel returns all of its columns to the conversation. After
viewing an inactive managed session, `/conversation archive` removes only Xana's
local retained handle; `/conversation archive ID` targets an exact retained
managed ID. Neither form deletes the Codex-owned thread. The active runtime
session and native journals cannot be archived through this command.
`/conversation new` stops the idle frontend owner and restarts through Xana's normal
composition boundary with the current resolved connection, model, permissions,
workspace, and profile. The previous session remains retained. Native Xana
creates a new durable session; managed Codex creates its vendor-owned thread on
the first turn. An active turn must finish or be interrupted first so this
command cannot create a competing workspace root. `/clear` remains different:
it clears the active owner's context rather than creating and navigating to a
separate session.

`/conversation continue` selects the latest compatible Conversation for this
workspace. `xana conversation continue` exposes the same lifecycle from the
shell. `xana conversation preview ID` and `xana conversation attach ID` likewise
provide the TUI's bounded read-only preview and exact idle attach semantics to
plain-terminal and scripted users.

`/session` and `/sessions` remain compatibility aliases. New help and
documentation use Conversation; Run is reserved for one execution attempt.
`/conversation preview ID` is read-only and does not acquire control. Preview
mode is labeled in both the row and composer status; submission remains disabled
until the user returns to the attached Conversation or explicitly attaches the
previewed one.

Unsent composer text, cursor and selection, staged images, selected vision route,
and queued follow-ups remain keyed to their exact Conversation while the TUI
switches execution owners. Xana saves the source draft before rebuilding the
owner and restores only the destination draft; it never carries unsent input
between Conversations or translates history between native and managed owners.
The state is frontend-local and process-bounded, not durable Conversation history.

`/espejo` opens the full-screen, terminal-native work and attention perspective.
Use `/espejo project` to limit it to the selected Conversation's Project, including
`Ungrouped` as a real scope. See [Espejo](espejo.md) for its evidence limits,
navigation, and accessibility contract.

Session rows show the optional local project name or `Ungrouped`. Project,
profile, skill, and plugin commands are deliberately executed outside
raw/alternate-screen mode:
Xana restores the terminal, invokes the same typed application command as the
top-level CLI, shows its accessible result, and re-enters the TUI. This keeps
registry, authority, movement, and recovery policy out of Ratatui while making
the complete lifecycle discoverable in Ctrl+P and usable from the keyboard.
An active turn must finish or be interrupted first.
Selecting another native conversation opens its committed history for
inspection while leaving an active root attached to its original conversation;
managed history remains owned by Codex. Drafting remains local, but Xana will
not submit a draft from a read-only historical view. Return to the runtime
conversation or use the exact resume command shown in [Sessions](sessions.md).
Native history initially loads at most 128 messages; scrolling to the older
edge requests another bounded page while preserving the current anchor.

## Layout and accessibility

On supported terminals, initialization first plays the skippable canonical
Xana portrait transition described in [Terminal presentation](presentation.md).
The header then starts expanded with Xana's wordmark, version, connection, model,
session, and status. Typing or pasting a draft collapses it to the compact
status bar. Click the header or use `/header view show` to expand it again.

Wide terminals show session, conversation, and activity columns. Medium and
narrow terminals prioritize conversation and composer content and show
activity as a drawer. `/activity view auto` shows it for substantive plans,
tools, children, commands, diffs, managed work, warnings, errors, or approvals
and hides it after the next submitted message. `view show` pins it; `view hide`
keeps a compact status, but cannot conceal an approval or critical failure.
The explicit mode is persisted in `data/frontend/presentation.toml`.

Activity cards retain execution ownership: Xana roots, Xana children, Codex
managed turns, and Codex-owned collaboration are not merged. Reasoning
summaries and raw reasoning are labeled separately from assistant messages and
appear only when the execution owner emits them. No display action asks a
model to summarize, changes reasoning effort, or adds tokens.
Native provider reasoning is incrementally accumulated into one bounded,
collapsed card per step. Committed assistant tool requests and tool results are
also projected into the conversation and activity panes immediately; restart
is not required to reveal durable work.
Click an activity card's summary row to expand or collapse its inline detail.
Click the expanded detail to open a bounded Activity Details modal. The modal
supports Up/Down and mouse-wheel scrolling, ordinary drag selection, Ctrl+C
copy, and Esc to close, so long reasoning/tool text remains readable without
enlarging the narrow activity pane. Selection and scrolling are frontend-only
state and never enter model context.
While an attached turn is active, the conversation tail shows a local animated
`Xana is working...` marker in addition to the status and activity panes. It is
presentation state only: it is never persisted or added to model context. The
animation advances at a low fixed rate, becomes static when reduced motion is
selected, and disappears on completion, interruption, or failure.

Approval cards show the requesting conversation or child, a user-facing action,
and one normalized exact scope summary. Enter chooses the highlighted
allow-once, exact-session,
or deny action; Esc leaves the correlated request pending. A decision is sent
exactly once through the existing permission/runtime callback. Approval cards
appear even when activity is hidden. One-shot mode continues to fail closed by
design.

The conversation is anchored to its newest visual rows. Mouse-wheel scrolling
moves by visual rows, including within one long wrapped message, instead of
skipping whole messages. On wide layouts, clicking a session inspects it,
clicking the sessions title hides that panel, clicking an activity summary
expands or collapses its detail, and clicking expanded activity content opens
the scrollable detail modal. Click or drag in the composer to place or
extend its selection, and click a selectable overlay row to activate the same
typed action as Enter.
An ordinary drag inside the conversation highlights its visible rendered cells
and retains that selection. Ctrl+C copies it to the platform text clipboard
without removing the highlight; an ordinary click elsewhere or Esc clears it.
When no conversation selection exists, Ctrl+C keeps its interrupt meaning.
Xana reports copy success or clipboard unavailability in the status line. Hold
Shift while dragging to use the terminal's native screen-text selection
anywhere instead. Full-screen terminal mouse capture is global rather than
panel-aware, so Xana implements conversation selection itself while retaining
mouse-down activation for panel headers, session rows, activity cards,
composer placement, and scrolling. Queued drag motion is replaceable input:
Xana renders the newest available pointer coordinate rather than animating
through stale intermediate positions.
Terminal resizing always recomputes both rendering and mouse hit targets from
the same responsive layout. These optional mouse conveniences grant no extra
authority. User message separators and text are right-aligned; Xana messages
are left-aligned; activity retains its owner-aware left-aligned cards. Color is
never the only state indicator. Theme, Unicode/ASCII,
reduced-motion, density, and composer preferences have safe fallbacks
documented in [Terminal presentation](presentation.md). Terminal state is
restored on normal exit, error, cancellation, and panic unwind. Closing the
embedded managed TUI cancels its correlated active Codex turn, resolves any
outstanding approval closed, and shuts down app-server.
