# M4 Desktop Espejo evidence

> Scope: M4-18 implementation evidence
>
> Status: Implementation complete; owner visual/accessibility verification remains in M4-24

## Implemented contract

- Desktop Espejo is a top-level retained feature view reachable from the fixed
  sidebar, native View menu, shared command palette, forwarded launch intent,
  and redacted host-notification flow.
- Global scope joins the bounded local application-host snapshot with Project
  navigation; selected-Project scope excludes other Projects. Ungrouped
  Conversations remain visible only in global scope.
- Pure classification separates Needs-you, in-motion, blocked/failed, recently
  completed, and idle work. Filters never mutate runtime state.
- Cards identify stable Conversation and workspace placement, native or managed
  execution owner, connection/model/Profile, permission mode, active Run or last
  outcome, controller state, pending approvals, queued input, and Activity count.
- Ordinary cards open the exact Conversation. Needs-you cards open its Activity;
  all approval and mutation authority is still enforced by the runtime.
- Global notices retain only bounded kind/code/correlation fields and route to
  Diagnostics. Conversation errors stay in the exact Activity view.
- Missing detailed artifact, usage, or receipt facts instruct the user to open
  the Conversation. No scheduler or `Coming up` section is rendered.

## Automated evidence

Run from the Xana repository root:

```console
cargo test -p xana-desktop --all-targets
cargo test -p xana --all-targets
cargo clippy -p xana-desktop --all-targets -- -D warnings
```

The focused suite covers global and Project scope, queue-aware classification,
host terminal-state reduction, notice deduplication and hard bounds, the eight-
Conversation/four-stream fixture, exact attention destinations, shared command
availability, and retained feature events. The full Xana library suite proves
the expanded Desktop host projection remains compatible with execution-host,
controller, navigation, notification, and semantic command contracts.

## Manual handoff

M4-24 retains the owner-only visual and assistive-technology pass. Exercise at
least eight Conversations across two or more Projects/workspaces with four mock
or real Runs, one approval, one failure, one completion, one ungrouped idle
Conversation, and one host notice. Verify pointer and keyboard navigation,
Project exclusion, filter clarity, scroll/reflow, 200% text scale, reduced
motion, screen-reader names, exact card destinations, and that returning from
Espejo neither clears attention nor mutates work.
