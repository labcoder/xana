# Xana Desktop

Xana Desktop is the native GPUI client included in the Xana Cargo workspace.
It embeds the matching Xana runtime in its own process, so it does not search
for or launch a `xana` executable from `PATH`.

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

To open the current repository directly while developing, name it explicitly:

```console
cargo run --locked -p xana-desktop -- --workspace .
```

The current Desktop supports native conversational providers, a persistent
Project/Conversation sidebar, graphical first-run setup, staged Settings, and
focused management and recovery views. Managed Codex conversation
presentation, multiple windows, and installation as a packaged application
remain unavailable.

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

An invalid, incompatible, or interrupted configuration opens **Diagnose and
recover**, not setup. Doctor remains read-only. A repair, migration, or reset
first produces an exact review plan and requires a separate commit. Migration
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
and Settings remain fixed at the bottom; their complete workspaces arrive in
later M4 slices.

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

The Message panel is currently a guaranteed layout anchor while the retained
`gpui-ai` composer remains inside Conversation. The next M4 Desktop slice
separates those two views without replacing the retained, virtualized chat or
its IME-capable composer.

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
the Conversation or Activity as the currently implemented views allow. See
[Logs and crash diagnostics](diagnostics.md) for configuration examples.

## Native external actions

Help can open Xana's fixed HTTPS documentation URL. Open Configuration and
Reveal Logs operate only on the exact paths resolved from `XANA_HOME`; missing,
non-regular, or symbolic-link targets are rejected. Desktop does not accept an
arbitrary URL or filesystem path from the command palette.

## Troubleshooting

- `configuration_unavailable`: run `cargo run --locked -- setup` with the same
  `XANA_HOME`, or use Desktop's Diagnose and recover view when the configuration
  is invalid, incompatible, or interrupted.
- `instance_unavailable`: the existing Desktop did not respond to authenticated
  forwarding. Close a hung process and retry; do not manually reuse the
  endpoint or capability from the descriptor.
- `workspace_unavailable`: choose an existing accessible directory or pass an
  explicit `--workspace PATH`.
- `protocol_mismatch`: rebuild the full workspace from one checkout.
- A disabled palette row explains which authority, configuration, active Run,
  or later Desktop slice it requires.
