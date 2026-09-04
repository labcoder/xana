# Xana Desktop

Xana Desktop is the native GPUI client included in the Xana Cargo workspace.
It includes the matching Xana runtime and owns it when the selected workspace
is unclaimed. If a compatible local foreground Xana host is already live,
Desktop attaches to that owner instead. It never searches for or launches a
`xana` executable from `PATH`.

## Start Desktop

Complete setup with the root application, then launch the Desktop package from
a source checkout:

```console
cargo run --locked -- setup
cargo run --locked -p xana-desktop
```

An argument-free, icon-style launch opens a read-only chooser. It offers active
Projects and valid recent workspaces/Conversations, or lets you choose a folder.
No Project or Conversation is inferred or created while this screen is open.
After choosing a folder, choose either **Open latest Conversation** or **New
ungrouped Conversation** so the lifecycle effect is explicit.

If that workspace is already hosted by `xana serve` or an attached terminal
frontend, Desktop authenticates over the host's private loopback boundary and
uses its authoritative snapshot. It requests control only when no incumbent
controller exists; otherwise mutation controls stay disabled and Desktop is an
observer. Desktop never takes over implicitly or starts a competing runtime.

To open the current repository directly while developing, name it explicitly:

```console
cargo run --locked -p xana-desktop -- --workspace .
```

The current Desktop supports native conversational providers and managed Codex,
a persistent Project/Conversation sidebar, graphical first-run setup, staged
Settings, and focused management and recovery views. Multiple windows and
installation as a packaged application remain unavailable.

`--catalog` opens the provider-free component review surface. `--open
conversation` and `--open activity` focus a named part of an already-running
Desktop instance or select it on first launch:

```console
cargo run --locked -p xana-desktop -- --catalog
cargo run --locked -p xana-desktop -- --open activity
cargo run --locked -p xana-desktop -- --workspace . --open conversation
```

Launch destinations are a closed list. A workspace path is accepted only from
the primary process's explicit `--workspace` option or native folder picker;
Xana never forwards a URL, path, prompt, command line, or credential to an
already-running process.

## Setup, Settings, and management

When configuration is missing, Desktop opens graphical setup before starting a
workspace runtime. Choose **Start with one connection**, **Full customize**, or
**Blank**. Connection setup validates the endpoint or managed executable and
credential/account state, refreshes the live model catalog, and only then
validates the selected model. The review is redacted and the runtime owns the
same atomic configuration transaction used by terminal setup.

Settings has twelve stable sections: Overview, Appearance, Notifications,
Connections, Profiles, Workbench, Permissions, Execution, Attachments and
media, Diagnostics, Capabilities, and Advanced. Ordinary scalar changes stay
in one process-local draft until Review and Apply. The review names source,
scope, effect timing, validation findings, and the exact durable owners being
changed. Discard writes nothing; a concurrent change stops the commit and
offers reload instead of overwriting another writer.

Focused graphical managers cover connection declaration and health, catalog
refresh and model selection, credential replace/remove/login, Projects,
Profiles, capability readiness, permission rules, media resource limits,
notification preferences, and Workbench defaults. Stored secret values never
enter Desktop snapshots, controls, logs, or accessibility text. Test and
refresh are explicit operations; cached facts are labeled rather than
presented as live observations.

The **Capabilities** view is authoritative status, not a generic configuration
editor. It distinguishes installed, enabled, available, permitted, selected,
and containment facts for Skills, Agent Plugins, MCP servers, external agents,
and focused routes. In this build, lifecycle changes for those advanced
integrations—as well as image-generation routes, outbound-decision history, and
operation reconciliation—remain in Xana's typed terminal management flow. The
command palette can open the relevant Desktop status view and labels that
limitation; it does not imply that navigation changed configuration.

An invalid, incompatible, or interrupted configuration or private-state record
opens **Diagnose and recover**, not setup. Cold launch checks recovery status
before it reads Project or recent-Conversation records, so a supported older
record version can never prevent the recovery window from opening. Doctor
remains read-only. A repair, migration, or reset first produces an exact review
plan and requires a separate commit. Migration
retains recovery copies, reset lists preserved state and confirms credential
deletion separately, and the bounded support export contains metadata only.
These operations are also available from Settings and through the equivalent
terminal commands.

## Projects and Conversations

The left sidebar shows user-created Projects with their workspace
Conversations plus a separate ungrouped Conversations section. Its native
filter handles labels while retaining stable Xana identities. Selecting a
Conversation cleanly replaces the runtime attached to the same Desktop window;
selecting a Project makes its workspace the destination for the sidebar's new
Conversation action. A new Conversation is ungrouped when no Project is
selected.

Use the sidebar's collapse control to switch between full and mini modes. That
presentation choice survives restart. Missing or identity-changed Project
workspaces remain visible with a status badge instead of disappearing. Espejo
and Settings remain fixed at the bottom.

Rename, archive/unarchive, move/ungroup, branch-at-point, and destructive
confirmation controls are available from the contextual **Actions** menu or by
right-clicking the navigation area after selecting a row. Project rename and
archive/restore change only Xana's local organization. Ungroup preserves the
Conversation and workspace. A move inside the same workspace reassigns the
existing Conversation; a move to another Project workspace requires a second
confirmation and creates a fresh linked Conversation while preserving the
source and copying no transcript automatically. **Branch** names the exact
latest committed source point before it creates and opens a new Conversation.
The same actions remain available through root `xana project` and
`xana conversation` commands.

## Workbench layouts

The content area is a resizable Workbench. Its default places Conversation
above Message and Activity to the right. Panel header buttons let you select a
tab, move the active panel into the first tab stack, dock it at any outer edge,
maximize or restore it, and close panels that are safe to close. These buttons
are ordinary focusable controls, so the same actions work with pointer or
keyboard navigation.

The panel bar reopens Summary, Artifacts, Usage, and Working Set. It also lets
you reset the selected Conversation, save the current arrangement as the one
user default, remove that default, or import/export a layout. Resolution is:

1. the selected Conversation's last valid layout;
2. the one saved user default; then
3. Xana's built-in recovery layout.

Resize changes are saved atomically after a short debounce. A corrupt,
unsupported, oversized, or impossible file falls back safely and leaves an
explanation in Activity. Shared layout files are inert `.toml`: before import,
Xana previews the bounded panel list and substitutes an unavailable placeholder
for an unknown future panel. They cannot contain messages, paths, prompts,
commands, credentials, or executable content.

Conversation and Message are independent panels over one selected Xana
Conversation. Conversation uses a retained, virtualized `gpui-ai` transcript;
Message owns the retained IME-capable composer. Switching Conversations keeps a
bounded UI entity per recent Conversation, so draft text, cursor/selection,
focus, transcript scroll, staged attachments, and queued follow-ups do not leak
into another Conversation.

The attachment picker accepts multiple local resources. You can also drag files
onto Message or use **Clipboard image**; every path or clipboard payload is
validated by the runtime before it becomes part of a draft. Validated
PNG/JPEG/GIF images can be submitted only when the selected route supports image
input. Other media remains an explicit `retained only` card, and submission
fails with an actionable explanation rather than disclosing it to a provider.
While a Run is active, a new submission becomes a visible queued follow-up.
Current native and managed owners do not advertise same-turn steering, so Xana
says so rather than silently treating a follow-up as a steer.

The model menu is populated from the selected connection's runtime-owned model
catalog. Managed Codex model and reasoning controls apply to later turns only
after the app-server acknowledges them, and the existing vendor thread remains
attached. A native model or Profile transition instead requires a fresh
Conversation; the prior history remains unchanged. Every transition reports its
effect in Activity.

Failed responses expose **Retry** only while Xana retains the exact bounded Run
input. **Regenerate** and edit actions copy text into Message for review and do
not mutate immutable history. Interrupt targets the exact active Run. Clear,
new, archive, branch, retry, and follow-up remain distinct lifecycle actions.

## Rich content and artifacts

Conversation renders bounded Markdown, fenced code, tables, and diffs through
the selectable `gpui-ai` transcript. Display math remains readable as bounded
LaTeX source; Desktop does not yet advertise a native formula renderer. Model
authored markup is untrusted: raw HTML and executable markup are escaped,
non-HTTP(S) or credential-bearing links are removed, and Markdown image syntax
never loads a remote resource. A safe HTTPS link is still an explicit click;
showing the message performs no network request.

Xana resources appear as typed attachment cards. An accepted immutable static
PNG, JPEG, or WebP may gain a thumbnail only after the runtime re-verifies its
complete length and content digest and checks the configured byte, pixel, and
edge limits. Only the newest eight eligible previews totaling at most 20 MiB of
source data and an estimated 32 MiB of decoded RGBA data remain admitted;
older previews fall back to their typed cards.
Animated raster, SVG, Lottie, audio, video, unknown, rejected, oversized, or
deleted resources keep a metadata card instead of being decoded optimistically.
Desktop currently advertises neither native audio/video playback nor rich math.

Activate a card to open **Artifacts**. The panel shows declared and detected
media types separately, validation state, source/derivative lineage, and the
source, freshness, selection, and authorization of each operation capability.
It can copy the opaque artifact reference, save a verified copy through the
native file picker, reveal the verified retained artifact, or open it with the
platform default application. These explicit actions return to the runtime for
complete digest and file-identity verification; save never overwrites an
existing destination. The card never exposes the backing store path.

Activity shows owner-qualified reasoning summaries, tools, approvals, child and
integration work, progress, execution facts, usage freshness, receipts, and
terminal failures. Approval cards carry exact runtime-issued identity and offer
allow-once, allow-session-scope, and deny; hiding or moving Activity does not
grant authority. The status bar continues to surface outstanding attention.

Espejo is a screen-level command center over the same bounded runtime-owned
facts. Global scope covers the current local application host; Project scope
excludes other Projects. Filters and groups separate Needs-you, in-motion,
blocked/failed, recently completed, and idle Conversations. Cards expose
owner-qualified execution, controller, queue, approval, and Activity summaries,
then navigate to the exact Conversation or its actionable Activity. Redacted
host notices link to Diagnostics. Espejo has no scheduler or `Coming up` fiction.

## Menus, palette, and shortcuts

Desktop uses conventional application, File, Edit, View, Conversation,
Window, and Help menus. Edit actions are native text actions. Menus, buttons,
shortcuts, and the command palette dispatch the same stable command IDs.

Open the searchable command palette with `Cmd/Ctrl+Shift+P`. It includes every
Desktop-projected command from Xana's shared registry. Commands whose required
authority, configuration, active Run, or Desktop view is unavailable remain
visible with a reason and cannot be activated.

The intentionally small shortcut set is:

| Action | Shortcut |
|---|---|
| Command palette | `Cmd/Ctrl+Shift+P` |
| Clear Conversation | `Cmd/Ctrl+Shift+K` |
| Interrupt active Run | `Cmd/Ctrl+.` |
| Minimize | `Cmd/Ctrl+M` |
| Quit | `Cmd/Ctrl+Q` |

Xana does not implement global hotkeys or a general keybinding remapper.

## One instance per Xana home

Only one Desktop application owns a canonical `XANA_HOME` at a time. A second
launch using the same home authenticates to the existing process over a
loopback-only, bounded forwarding channel, asks it to focus or navigate, and
then exits successfully. Different Xana homes remain independent.

The instance descriptor contains a random capability and a loopback endpoint.
It is stored beneath Xana's runtime directory, carries no prompt or credential,
and is replaced after a stale owner lock is recovered. Forwarding is not a
general local control API.

The Desktop instance lease and the workspace execution-host lease are distinct.
One Desktop process may present an external compatible foreground host without
owning that host; closing the attached Desktop detaches its client and does not
stop the terminal-owned runtime.

## Closing, status, and notifications

Closing an idle last window requests runtime shutdown and waits for Xana's
ordered shutdown acknowledgment before removing the window. If a Run is still
active, Desktop asks whether to keep Xana open, cancel work and quit, or return
to the Conversation. A runtime failure remains visible instead of being
reported as a clean close.

The bottom status bar stays compact: it reports host lifecycle, current view,
active-Run count, pending approvals, global-notice count, and the latest
bounded activity label. Detailed failures belong in Activity and Diagnostics,
not in message history.

When Desktop is unfocused, the existing `[notifications]` configuration may
produce native notifications for approvals, questions, completions, failures,
controller loss, and host failures. Notification text is fixed and redacted:
it never includes prompts, model output, reasoning, filenames, tool arguments,
or credentials. Activating a notification focuses Xana and routes attention to
the exact Conversation, Activity, or redacted Espejo/Diagnostics path. See
[Logs and crash diagnostics](diagnostics.md) for configuration examples.

## Native external actions

Help can open Xana's fixed HTTPS documentation URL. Open Configuration and
Reveal Logs operate only on the exact paths resolved from `XANA_HOME`; missing,
non-regular, or symbolic-link targets are rejected. Desktop does not accept an
arbitrary URL or filesystem path from the command palette.

## Troubleshooting

- `configuration_unavailable`: run `cargo run --locked -- setup` with the same
  `XANA_HOME`, or use Desktop's Diagnose and recover view when configuration or
  private state is invalid, incompatible, migratable, or interrupted.
- `instance_unavailable`: the existing Desktop did not respond to authenticated
  forwarding. Close a hung process and retry; do not manually reuse the
  endpoint or capability from the descriptor.
- `workspace_unavailable`: choose an existing accessible directory or pass an
  explicit `--workspace PATH`.
- `protocol_mismatch`: rebuild the full workspace from one checkout.
- A disabled palette row explains which authority, configuration, active Run,
  or later Desktop slice it requires.
