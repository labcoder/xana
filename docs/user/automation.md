# Plain and one-shot terminal modes

> Audience: People using Xana interactively, from shell pipelines, or from scripts.

Xana exposes two permanent append-only surfaces before the full-screen TUI:

```text
xana --plain
cargo run -- --plain
```

Plain mode retains interactive commands, approvals, activity, and cancellation.
On exit it prints the native session id and exact installed/source-checkout
resume commands. `--tui` is reserved for the adaptive full-screen surface and
currently fails visibly rather than silently changing terminal behavior.

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
