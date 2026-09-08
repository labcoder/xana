# Changing settings without losing a conversation

> Audience: People configuring Xana while retaining their work.

You do not need a new native Conversation merely to enable web tools, update
Xana, edit permissions or change a compatible native model. History and the
Conversation ID stay intact. The original Profile is historical identity;
subsequent turns get their own recorded execution settings.

## What happens when settings change

| Situation | Behavior |
| --- | --- |
| Edit the current Profile or web configuration while idle | The next new native turn resolves and validates current settings. |
| Edit settings during an active tool turn or approval | That operation keeps its original tools and policy; the next turn uses the update. |
| Use `/setup`, `/settings` or native `/model` | Xana recomposes compatible execution and returns to the exact same Conversation. |
| Quit and reopen an idle Conversation | Current compatible settings apply; no new ID or history copy is created. |
| Invalid settings or missing native model | Admission fails with a settings error. Repair the configuration and retry here; history is not replaced. |
| Restart with unfinished work and changed settings | Continue is rejected if the original inputs cannot be reconstructed. Stop or explicitly reconcile that operation, then send a new turn here. |
| Change from native execution to managed Codex, or change workspace | This is a different execution owner/boundary, not a settings refresh. No automatic history translation occurs. |

Settings preparation performs no chat-provider call. MCP activation can perform
its existing bounded local startup/handshake. A failed refresh admits no new
operation. Preparation has a ten-second timeout; control commands interrupt it.
Allow-for-session grants are cleared when policy is replaced, and explicit
denials remain. Configuration updates cannot approve a pending tool request.

`xana doctor` checks retained-conversation configuration readiness without
changing it. It does not prove provider reachability, model answer quality or
that another process is not controlling the Conversation.

## Client and owner boundaries

Native tool/prompt/policy refresh happens before each new turn, not each token
or tool invocation. Unchanged configuration reuses the existing Agent and
provider connections. No polling or refresh work is added to rendering.

Client-owned presentation, attachment preparation, model catalog views and
installation services such as the browser owner are constructed when a client
attaches. Use `/setup` or `/settings` and return, or reopen the same Conversation,
after changing those settings externally. This refreshes client state; it does
not require creating a new Conversation. Managed Codex keeps its own inner loop;
use its existing in-thread model/reasoning controls or return from setup/settings
to apply other compatible launch changes to the same vendor thread.

The global *default Profile* selects future Conversations; existing Conversations
continue to resolve their own named Profile. Explicit Profile continuation and
`/conversation new` remain available when a new identity is actually intended.

## Interrupted work

In the TUI, inspect the round-budget decision and choose **Stop** when the old
operation should not continue. Ctrl+C interrupts active work (or copies a current
text selection; click away first). `/clear` is not a substitute for settling an
active or suspended operation. Use `xana operation --help` for explicit recovery
of other unfinished outcomes. No failed or uncertain tool action is silently
repeated as part of reopening or changing settings.

From a source checkout, replace `xana ...` with `cargo run -- ...`.
