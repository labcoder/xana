# Terminal and one-shot modes

> Audience: People using Xana interactively, from shell pipelines, or from scripts.

## Installation readiness

`xana setup --if-needed` is the stable checked-in installer handoff. With
redirected input or output it never prompts. Healthy setup exits `0`; required
setup exits `10` after a final `XANA_SETUP_RESULT` JSON receipt; indeterminate
filesystem or unexpected setup failures exit `1`. Exit `10` means the binary
operation may still be successful while configuration remains pending—it must
not be collapsed into either generic success or generic failure by an
installer. The receipt is versioned for Xana's checked-in wrappers but is not a
general third-party configuration API.

Bare `xana` selects the full-screen TUI when stdin and stdout are interactive.
Redirected or piped launches select the permanent append-only surface and emit
no terminal control sequences. Select either behavior explicitly with:

```text
xana --plain
cargo run -- --plain
xana --tui
cargo run -- --tui
```

Plain mode retains interactive commands, approvals, activity, and cancellation.
On exit it prints the native session id and exact installed/source-checkout
resume commands. The native TUI supports a bounded multiline composer, safe
paste preview, attachments, ordered busy follow-ups, correlated interruption,
a shared slash-command/command-palette registry, model selection, activity
visibility, streamed output, and adaptive layouts. Ctrl+C interrupts the exact
active turn and Ctrl+Q shuts down. Managed Codex chat uses plain mode until its
full-screen event and approval projection is attached. See the
[full-screen TUI guide](tui.md) for portable keys and interactive approvals.

Plain chat accepts `/project ...`, `/profile ...`, `/skill ...`, and `/plugin
...`. Xana stops the current idle execution owner, runs the exact typed command
used by the matching `xana` subcommand, prints its normal result, and resumes
plain chat. This includes
quoted arguments, lifecycle operations, readiness, profile resolution, and
review/apply continuation placement; malformed or oversized control commands
fail without changing project/profile state.

`--tui` requires interactive stdin and stdout and a successful terminal
initialization; it restores partial terminal state and exits nonzero instead
of silently falling back. An implicit bare launch restores first, warns on
stderr, and falls back to plain if the full-screen terminal cannot initialize.
Raw mode, alternate screen, cursor, mouse capture, and bracketed paste have one
idempotent cleanup owner for normal exit, error, EOF, cancellation, and panic
unwinding.

## Exactly one turn

Pass one prompt as an argument or through stdin, never both:

```text
xana -p "summarize this workspace"
Get-Content README.md | xana --print

cargo run -- -p "summarize this workspace"
Get-Content README.md | cargo run -- --print
```

Text mode writes only final assistant text to stdout. Loading, activity,
warnings, and diagnostics use stderr, so redirecting stdout captures a clean
payload. One-shot mode denies any approval that requires an interactive
decision and exits instead of waiting. A configured rule or permission mode
may authorize an effect before an approval is needed.

One-shot creates a new native session or managed thread by default. Use
`--continue` to select the latest compatible conversation for the canonical
workspace and execution owner. Use `--resume SESSION_ID` for one exact native
session. Xana never translates native history into a Codex thread or the
reverse, and exact native resume fails if launched from a different canonical
workspace.

## JSON and process status

`--output json` and `--json` emit one redacted version-1 envelope to stdout:

```json
{"version":1,"status":"success","result":{"text":"...","execution_owner":"native","session_id":"..."}}
```

Failures use `status: "error"` with an `error.category` and bounded message.
Diagnostics remain on stderr. JSON and redirected output contain no terminal
control sequences.

| Exit | Category | Meaning |
|---:|---|---|
| 0 | success | The one turn completed. |
| 2 | `invalid_input` | CLI syntax or the prompt source was invalid. |
| 3 | `configuration` | Xana is not initialized or configuration is invalid. |
| 4 | `connection` | Authentication, model availability, or provider connection failed. |
| 5 | `approval` | The turn required an unavailable interactive approval. |
| 6 | `runtime` | Provider or runtime execution failed. |
| 130 | `interrupted` | The operation was interrupted. |

The envelope is a result contract, not an event stream. Use stderr for human
diagnostics; future attached clients use Xana's bounded frontend protocol.
