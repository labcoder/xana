# Local foreground host and observer attachment

> Audience: Xana users

`xana serve` starts an explicit foreground host for the canonical current
workspace. It binds only to a loopback address, does not daemonize, and stops
when the process receives Ctrl+C. Course 1 does not support LAN or remote
attachment.

```text
xana serve
```

Use `--port PORT` to request a local port. `--bind` accepts a loopback address
only; a non-loopback address is rejected before Xana writes discovery state.
From a source checkout, place arguments after Cargo's separator:

```text
cargo run -- serve
```

The host writes a workspace-scoped runtime descriptor beneath Xana's runtime
directory. It contains the loopback endpoint, a per-launch host identity, and
a fresh capability. On Unix, Xana sets the directory to `0700` and the
descriptor to `0600`. On Windows, the normal runtime location is beneath the
current user's application directories and inherits that user's ACL. An
explicit `XANA_HOME` inherits the ACL of the directory you selected. The
capability is still checked on every platform; filesystem protection is
defense in depth.

The capability is never printed, placed in a URL, or passed in process
arguments. `xana attach` discovers and reads it from the descriptor, then sends
it in the first bounded WebSocket frame:

```text
xana attach
```

Attachment is workspace-specific. Run the command from the same canonical
workspace as the host. A stale, malformed, wrong-workspace, wrong-version,
non-loopback, or unauthorized descriptor fails before snapshot data is sent.
Browser WebSocket handshakes, when used, must have a loopback `Origin`; the
native CLI does not send an Origin header.

## Observer behavior

The currently shipped attached role is passive observer. It receives one
bounded atomic JSON snapshot followed by ordered JSON observation lines.
Snapshots expose a hash of the canonical workspace identity, a bounded display
name, and at most 512 conversation summaries. They do not expose the canonical
filesystem path, API keys, credential references, Codex OAuth data, or the
capability. Each wire frame is limited to 1 MiB and each observer has a
256-event queue.

Observers cannot submit turns, cancel work, answer approvals, change
configuration, or acquire control. A mutation attempt receives a correlated
rejection and emits a bounded audit observation without reaching the runtime.
Closing an observer does not cancel work. Controller acquisition and hosted
turn execution are not part of this observer-only slice yet.

Snapshot capture and subscription share one host lock. An event is therefore
either represented by the snapshot boundary or delivered afterward, never
lost between the two. Sequence gaps and reconnects require a fresh snapshot;
the local stream is not durable replay.

The protocol is repository-private and versioned for Xana's own frontends. It
is not a public SDK or compatibility promise. There is no daemon discovery,
automatic startup, TLS, remote authentication, or LAN binding.
