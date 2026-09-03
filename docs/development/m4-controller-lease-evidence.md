# M4 controller lease evidence

> Scope: M4-08 controller/observer coordination
>
> Status: implementation complete; cross-platform CI pending an authorized push
> Implementation: Xana `94499a1`, Desktop attach adapter `3bbc131`

## Implemented contract

`controller` is one transport-independent, deterministic lease reducer used by
the application execution host and authenticated loopback host. It keys
authority by Conversation and projects only:

- a non-secret controller ID and monotonically advancing generation;
- connected or reconnecting state;
- confirmed-takeover state;
- disconnect reason and monotonic reconnect-grace remainder; and
- typed acquired, renewed, reconnected, taken-over, disconnected, released, and
  expired transitions.

The reconnect bearer is random, retained only as a BLAKE3 digest by the lease,
rotated on acquisition, renewal, and reconnect, zeroized by its transport
wrappers, and excluded from snapshots, events, diagnostics, Debug output, and
durable state. Authentication remains separate from Conversation authority.

Takeover is not a last-writer-wins Boolean. Confirmation binds the exact
controller ID and generation observed in an authoritative snapshot. The first
valid challenger advances the generation, making every competing stale
confirmation fail with the replacement lease. Any pending native or managed
approval blocks takeover. Observers and stale controllers cannot submit,
interrupt, clear, answer approvals, or otherwise mutate runtime state.

The Desktop embedded backend acquires its own controller before publishing its
initial snapshot, checks that controller for every mutating command, exposes a
bounded presentation-safe controller projection, and releases it during clean
shutdown. The loopback attach client renews its rotating reconnect capability,
recovers through a fresh snapshot, and fails closed on release or grace expiry.
A new authenticated transport can replace a stalled socket with the current
capability without briefly authorizing both clients.

Desktop startup now also discovers a compatible live foreground workspace host
before launching an embedded owner. It authenticates to that host, requests
only an unclaimed controller lease, and otherwise projects observer authority.
An incumbent controller remains unchanged, mutating Desktop controls fail
closed, and closing Desktop detaches the client without shutting down the
external host.

## Deterministic evidence

- Unit-clock fixtures cover acquire, renew, transport replacement, disconnect,
  reconnect, monotonic expiry, stale capability rejection, and zero authority
  after host reconstruction.
- An exact-generation race fixture proves one takeover winner and one visible
  stale loser.
- Application-host fixtures prove independent controllers for two
  Conversations and typed cross-Conversation rejection.
- Native pending-approval and loopback native/managed approval state block
  takeover before mutation.
- Real loopback fixtures prove explicit acquisition, observer rejection,
  takeover, stale-controller rejection, release fail-closed behavior, and fresh
  snapshot sequencing.
- Desktop integration proves the initial controller projection, a complete
  streamed turn, and lease release on shutdown.
- Real loopback Desktop fixtures prove unclaimed-controller command routing and
  prove that an incumbent terminal controller remains controller after Desktop
  attaches.
- Controller changes emit ordered host observations and metadata-only
  Diagnostics facts without bearer material.

The complete local verification gate passed on Windows:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

The all-feature suite passed 1,002 tests with 6 documented live/stress tests
ignored, plus 20 CLI, 4 settings CLI, and 3 Desktop tests. The no-default suite
passed the corresponding complete workspace gate. Linux and macOS remain the
ordinary CI evidence boundary at the next authorized push.

## Deliberate limits

This is local coordination, not user identity, collaboration, remote authority,
shared editing, or provider-native controller semantics. One attached client
controls or observes one selected Conversation while `ExecutionHost` owns the
bounded multi-Conversation registry. Multi-window shared editing and remote
coordination remain outside M4; current clients consume this lease contract
instead of reimplementing it.
