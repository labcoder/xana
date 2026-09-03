# M4 Desktop native lifecycle evidence

This record covers the M4-15 native command, single-instance, notification,
external-open, status, and close-lifecycle slice. It records automated evidence
separately from owner-only visual and platform checks.

## Command convergence

- The shared catalog owns every stable Desktop command ID.
- `commands.rs` projects exactly one searchable row per Desktop catalog entry.
- Native menus, palette selections, buttons, and bounded shortcuts converge on
  `WorkbenchCommand` and its stable ID before dispatch.
- Commands outside the current Desktop surface remain discoverable only when
  the shared catalog has an honest disabled reason.
- The essential shortcut set is bounded to five unique application shortcuts;
  native Edit items continue to use platform text actions.

Automated coverage checks unique IDs and shortcuts, catalog/palette
completeness, stable-ID round trips, and disabled future commands.

## Instance and forwarding boundary

The instance coordinator is owned below the GPUI crate by `xana::desktop`.
It uses:

- an exclusive owner lock beneath the canonical Xana runtime directory;
- an atomic private descriptor with protocol versions, process ID, canonical
  instance root, loopback endpoint, and a random 256-bit capability;
- a loopback-only listener, 4 KiB request/response bounds, two-second I/O
  bounds, and a 16-entry intent queue; and
- a closed `Focus | Navigate(named destination)` payload with no arbitrary
  command, prompt, URL, path, or credential.

Tests cover same-home forwarding, distinct homes, canonical path aliases, a
six-launch race with one elected owner, stale descriptor replacement, hostile
extra fields/wrong capability, and rejection of path/URL-shaped navigation.

## Lifecycle and platform adapters

- Idle close requests shutdown without blocking GPUI and removes the window
  only after the runtime publishes an expected stop.
- Active close presents keep-open, cancel-and-quit, and return choices.
- The retained Workbench sleeps on coalesced runtime and forwarded-launch wake
  signals, drains at most 64 updates per foreground batch, and has no settled
  polling or repaint clock.
- A selected workspace attaches to a compatible live foreground owner before
  Desktop may start an embedded runtime. Desktop acquires only an unclaimed
  controller and closing an attached window never stops the external owner.
- Notification policy comes from the loaded Xana configuration. The existing
  focus-aware planner creates fixed redacted candidates; GPUI receives no raw
  message, reasoning, filename, tool argument, or credential.
- Documentation opening uses one fixed HTTPS origin. Configuration and log
  actions inspect only exact Xana-resolved regular-file/directory targets on a
  background executor, reject missing or symbolic-link targets, then return the
  validated fixed action to GPUI.
- The status row projects only host lifecycle, selected destination, active-Run
  count, approval count, notice count, and bounded activity text.

## Automated verification

The implementation was checked with:

```console
cargo fmt --all -- --check
cargo clippy -p xana-desktop --all-targets -- -D warnings
cargo test -p xana-desktop --all-targets
cargo test --lib desktop:: -- --nocapture
```

The complete workspace verification matrix is run before the implementation
commit is finalized.

## Owner-only manual verification still required

Run on Windows, Linux, and macOS where available:

1. Start Desktop, then launch a second process with the same `XANA_HOME` and
   verify that the first window focuses while the second exits successfully.
2. Launch with a different `XANA_HOME` and verify that a second independent
   window opens.
3. Start a foreground terminal host for the same workspace, then launch
   Desktop. Verify Desktop attaches, does not displace an incumbent controller,
   and detaches without stopping the terminal host.
4. Open native menus and the command palette using pointer, keyboard, and a
   screen reader. Confirm disabled reasons and platform-native Edit behavior.
5. Start a slow Run, close the last window, and exercise all three close
   choices. Confirm there is no zombie Xana runtime.
6. Unfocus/minimize Desktop and trigger an enabled completion and failure.
   Confirm notification copy is redacted and activation focuses Xana.
7. Verify Open Configuration, Reveal Logs, documentation, Minimize, Clear, and
   Interrupt. Confirm unsupported palette rows cannot activate.

These checks remain open until the owner evaluates native behavior and visual
quality on the supported platforms.
