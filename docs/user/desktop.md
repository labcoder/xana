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

The current Desktop slice supports native conversational providers. Managed
Codex presentation, the complete sidebar and settings workspace, multiple
windows, and installation as a packaged application remain unavailable.

`--catalog` opens the provider-free component review surface. `--open
conversation` and `--open activity` focus a named part of an already-running
Desktop instance or select it on first launch:

```console
cargo run --locked -p xana-desktop -- --catalog
cargo run --locked -p xana-desktop -- --open activity
```

Launch destinations are a closed list; Xana never accepts a forwarded URL,
path, prompt, command line, or credential.

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
  `XANA_HOME`.
- `instance_unavailable`: the existing Desktop did not respond to authenticated
  forwarding. Close a hung process and retry; do not manually reuse the
  endpoint or capability from the descriptor.
- `workspace_unavailable`: launch from an existing accessible directory.
- `protocol_mismatch`: rebuild the full workspace from one checkout.
- A disabled palette row explains which authority, configuration, active Run,
  or later Desktop slice it requires.
