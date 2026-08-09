# Local foreground host, observers, and control

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

The default attached role is passive observer. It receives one
bounded atomic JSON snapshot followed by ordered JSON observation lines.
Snapshots expose a hash of the canonical workspace identity, a bounded display
name, and at most 512 conversation summaries. They do not expose the canonical
filesystem path, API keys, credential references, Codex OAuth data, or the
capability. Each wire frame is limited to 1 MiB and each observer has a
256-event queue.

Observers cannot submit turns, cancel work, answer approvals, or change
configuration. A mutation attempt receives a correlated
rejection and emits a bounded audit observation without reaching the runtime.
Closing an observer does not cancel work.

## Explicit controller behavior

One attached client may explicitly control the conversation hosted by this
foreground process:

```text
xana attach --control
xana attach --control --prompt "explain the failing test"
```

From a source checkout, use `cargo run -- attach --control`. Controller
acquisition is never inferred from connection order. A second client remains
an observer unless it requests `--takeover`, which is explicit and appears in
the ordered control audit stream. Exactly one controller can submit commands
or answer correlated native or managed approvals. The role grants no direct
filesystem, credential, provider-wire, configuration, or host-administration
access.

The controller receives a per-lease reconnect capability in memory. If its
socket drops, Xana marks the lease reconnecting for three seconds. A reconnect
uses a fresh atomic snapshot; it does not trust client-local history. While
reconnecting, no client can advance an approval. If grace expires, Xana denies
or cancels every pending native/managed approval and interrupts the active
root. Observers never inherit authority. Releasing control also fails closed
before the role becomes observer.

Native and managed Codex hosts use the same controller envelope. Codex still
owns its thread and inner tool loop; Xana submits correlated turns, projects
provider-neutral activity, and returns only exact one-effect approval choices.
Managed controller attachment intentionally rejects path-bearing image input
and unsupported native-only commands.

Snapshot capture and subscription share one host lock. An event is therefore
either represented by the snapshot boundary or delivered afterward, never
lost between the two. Sequence gaps and reconnects require a fresh snapshot;
the local stream is not durable replay.

## Artifact previews

An attached observer or controller may request a preview only by the immutable
artifact id already present in that host's visible conversation:

```text
xana attach --artifact ARTIFACT_ID
```

Xana never accepts a filesystem path through this endpoint. The host keeps at
most 512 visible artifact registrations, re-verifies content length and BLAKE3
digest from the immutable store, and returns at most 64 KiB plus bounded media
metadata and a truncation flag. Unknown, evicted, corrupt, or not-visible ids
all fail without revealing a storage path. Each request is authenticated by
the same host capability as the snapshot.

## Resource and shutdown bounds

The host accepts at most 32 simultaneous clients. Each client has a 256-event
outbound queue, may send at most 256 frames per second, and uses 1 MiB frame
and two-second outbound-write limits. A slow reader, oversized frame,
malformed request, or rate overflow disconnects only that client; execution
and other observers do not wait for it. Reattachment always starts from a
fresh snapshot.

Ctrl+C stops intake and announces shutdown. Xana first fails pending approval
work closed, requests native/managed cancellation, and waits up to two seconds
for ordinary cleanup. At the five-second hard bound it aborts only the exact
host-owned execution task. Native runtime/Phase 4 child shutdown and
`kill_on_drop` process ownership then reap their owned work; Codex app-server
also receives its normal two-second exit opportunity before exact child
termination. Xana never kills a process selected only by a stale PID or broad
process name. The verified descriptor lease is removed when the host exits.

The protocol is repository-private and versioned for Xana's own frontends. It
is not a public SDK or compatibility promise. There is no daemon discovery,
automatic startup, TLS, remote authentication, or LAN binding.
