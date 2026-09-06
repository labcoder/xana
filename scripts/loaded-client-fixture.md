# Loaded-client fixture

This development helper uses Node built-ins and an already-built Xana binary.
It creates only synthetic state below `target/loaded-client-fixtures/`. It does
not build Xana, import a journal, contact a model service, change an OS keychain,
or delete fixtures. The generated recovery key is intentionally retained inside
this disposable fixture; never use it for real data or commit generated files.

## Prepare once

From the checkout, run:

```powershell
node --test scripts/prepare-loaded-client-fixture.test.mjs
node scripts/prepare-loaded-client-fixture.mjs --prepare target/debug/xana.exe
```

Preparation creates a unique directory and manual-custody protected store, then
uses the real CLI and a loopback-only synthetic SSE provider for 320 turns. It
compacts, verifies at least 640 durable messages, closes/restarts the provider on
the same route, resumes the exact session, and appends another turn. A 240-frame
slow stream runs while a separate CLI process reads a bounded history preview.
The final inspection must retain at least 644 messages, and storage verification
must pass. No raw prompt/response or recovery-key content is logged by the helper.

Keep the printed fixture directory. Its `fixture.json` records the exact binary
hash, session, message count and synthetic-only checks. A failed preparation is
not a usable fixture; its create-only directory remains for diagnosis. Re-run to
create a new directory. The script does not overwrite or clean up prior runs.

## Serve, then launch a client

In one terminal, start the provider with the printed absolute directory:

```powershell
node scripts/prepare-loaded-client-fixture.mjs --serve C:/ABSOLUTE/loaded-FIXTURE-ID
```

It binds only `127.0.0.1`, accepts a per-fixture route token, limits request bodies
to 512 KiB, caps active streams, emits bounded frames and stops after one hour.
Ctrl+C closes its owned sockets. Stop a client before stopping the provider unless
you are deliberately testing provider interruption. Restart `--serve` for the
same fixture before resuming; the server owns no conversation state.

In a second terminal, substitute that same absolute directory:

```powershell
$loadedFixture = Get-Content -LiteralPath 'C:/ABSOLUTE/loaded-FIXTURE-ID/fixture.json' -Raw | ConvertFrom-Json
$env:XANA_HOME = $loadedFixture.home
$env:XANA_STORAGE_RECOVERY_KEY = $loadedFixture.key
Set-Location -LiteralPath $loadedFixture.workspace
& $loadedFixture.binary --tui --resume $loadedFixture.session
# Or, using a matching already-built Desktop binary:
& 'C:/ABSOLUTE/xana/target/debug/xana-desktop.exe' --workspace $loadedFixture.workspace
```

These environment changes affect only this dedicated terminal. Close it when
finished. In Desktop choose the existing fixture Conversation, not New. Verify
the session against the manifest before submitting. Do not run a real provider
or substitute a real home. Use only `fixture:slow` or `fixture:next` as submitted
text; other prompts are rejected by the fixture provider. Paste/selection drafts
may be arbitrary synthetic text, but discard them before submitting.

## Record manual results, not inferred passes

1. Scroll past the initial 128-message page and through more than 512 messages.
   Select/copy multilingual text, paste a multiline draft, resize, switch away
   and back, and confirm the draft, scroll anchor and tail-follow behavior.
2. Submit `fixture:slow`. During its roughly 24-second stream, browse older
   history, resize and inspect Activity. Clear any selection before TUI Ctrl+C,
   or use `/interrupt`; Desktop uses Interrupt or Ctrl+period. Confirm streaming
   stops and the exact operation reports its outcome (native interruption can
   retain a recoverable Suspended operation), then submit `fixture:next`. Record
   whether it completes without duplicate output or a stuck busy state.
3. With the client idle, invoke Compact Conversation (`/compact` in TUI). Inspect
   progress and, if still running, interrupt it; then submit `fixture:next`.
   The deterministic compactor can finish too quickly for a human to cancel.
   That is an unexercised cancellation check, not a pass. The separate headless
   loaded-owner test proves blocking-source cancellation causally; it does not
   measure native input responsiveness. Repeat the client test with a qualified
   semantic helper only after its separate approval; this fixture enables none.
4. Quit the client and server, restart the server, resume the same Conversation,
   and confirm history/continuation. Record app revision/hash, OS/hardware,
   terminal/display, actual interactions, failures and measurement method.

Native scroll/clipboard/focus, frame rate and warm non-model control p95 under
concurrent maintenance require owner/reference-system observation. Automated
provider cancellation is not a client cancellation pass. Windows evidence does
not establish native macOS or ordinary-user Linux results.

## Optional backend measurement

With the client stopped, run:

```powershell
node scripts/prepare-loaded-client-fixture.mjs --measure C:/ABSOLUTE/loaded-FIXTURE-ID
```

This runs twenty **fresh CLI process** bounded previews and records p50/p95 in a
create-only JSON file in the fixture directory. It includes process startup and
manual-key/store open. It is neither a warm live-client control benchmark nor a
concurrent-maintenance or FPS result. A changed CLI binary requires preparing a
new fixture so the recorded identity remains exact.
