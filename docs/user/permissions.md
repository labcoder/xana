# Xana permissions

> Audience: People installing, configuring, or using Xana.

Every built-in tool invocation crosses one permission broker before it can
read, write, or execute. The broker combines the configured default, matching
user-owned rules, and memory-only session grants. Tools and model output cannot
approve themselves.

## Default modes

`permission_mode` is required and applies when no rule matches:

| Mode | Behavior |
|---|---|
| `deny` | Reject the tool invocation without performing its effect |
| `ask` | Ask the controlling terminal for a decision; this is the initializer default |
| `allow` | Run the effect automatically with Xana's host permissions |

Existing version 1 files that explicitly use `allow` remain valid and retain
automatic authority. To adopt the safer interactive default, change the value
to `ask` and run `xana config check`.

Full Custom Setup and `xana setup --section permissions-shell` edit the
default, validated rules, and shell without mutating an open conversation. The
noninteractive rule form is `--permission-rule
ID:DECISION:EFFECT[:WORKSPACE]`; multiple flags append multiple rules. The
receipt requires a new conversation because its permission and shell snapshot
is immutable for the current owner.

## Rule matching and precedence

Rules are top-level TOML array entries. Each rule needs a unique, nonblank id,
a decision, and at least one matcher:

```toml
permission_mode = "ask"

[[permission_rules]]
id = "deny-git-secrets"
decision = "deny"
tool = "read_file"
workspace = ".git"

[[permission_rules]]
id = "allow-project-reads"
decision = "allow"
effect = "read"
workspace = "."

[[permission_rules]]
id = "ask-tests"
decision = "ask"
tool = "run_command"
command = "cargo test"
```

Matchers are conjunctive within a rule. A rule with both `tool` and `workspace`
must match both. Across all matching rules, precedence is independent of file
order:

```text
any deny -> deny
else any ask -> ask
else any allow -> allow
else permission_mode
```

Supported effects are `read`, `write`, `execute`, `network`, and `external`.
Current tool names are `read_file`, `list_files`, `edit_file`, `run_command`,
`read_document`, and `xana_docs`. Workspace matchers are relative to Xana's launch workspace;
they are resolved to existing canonical paths when chat starts. Absolute,
missing, escaping, and parent-traversing workspace rules fail startup. Command
matching is an exact string comparison, not shell parsing or wildcard syntax.

## What an ask means

For `ask`, the terminal shows the tool, effect, normalized final arguments,
and canonical scope. The available decisions are:

- deny;
- allow this invocation once; or
- allow the same tool and effect for the current session scope.

A workspace session grant covers the same canonical path or a path beneath it.
A command grant requires the same selected shell, canonical working directory,
and exact command. The terminal cannot widen the scope in the request, and a
session grant cannot override a matching deny rule or a default deny. Exact
duplicate grants reuse one entry; at most 256 distinct grants can exist in a
process. Grants exist only in process memory and disappear when Xana exits.

Blank input, EOF, an unknown or stale decision, controller loss, and an
unattended ask all deny. A pending request is correlated by operation and tool
invocation ids. `allow once` never authorizes a later invocation.

An explicit `xana attach --control` client may answer only approval ids emitted
for the one conversation it controls. Observers, stale controllers, duplicate
answers, and ids from another native child, operation, or managed callback are
rejected. Disconnect starts a three-second authenticated reconnect grace;
expiry and explicit release deny/cancel pending requests and interrupt the
active root. Takeover is explicit and does not widen the underlying policy.

One-shot mode is noninteractive: a request that reaches the approval boundary
is denied and the process exits with the `approval` category instead of
waiting. Explicit configured policy may authorize an effect before a request
is necessary. See [Plain and one-shot modes](automation.md).

## Managed Codex approvals

A Codex connection owns its inner tool loop, sandbox policy, and approval
semantics. Xana translates the command-execution and file-change approval
requests emitted by app-server into an explicit terminal choice and returns
the correlated decision. Rendering a plan, reasoning summary, command output,
diff, or other managed activity is observation only and never grants
authority.

Foreground Codex approval choices remain local to that managed conversation;
they do not become native Xana grants. A supervised Codex child instead routes
each callback through that child's existing Xana permission broker, preserving
parent, child, route, operation, and invocation correlation. An effective
`deny` mode makes a managed Codex child route unavailable because the current
app-server contract cannot prove that every inner tool effect is disabled;
Xana fails closed instead of presenting read-only sandboxing as equivalent to
zero tool authority. `ask` selects workspace-write plus on-request approval.
`allow` selects workspace-write with no prompt. No child permission mode
selects Codex's danger-full-access sandbox, and the child route and request may
only narrow the root's authority ceiling.

Xana owns the lifetime and scope of its in-process session grants. Even when a
matching Xana grant authorizes a later callback without another terminal
prompt, Xana returns only Codex's one-effect `accept` decision. It never returns
`acceptForSession`, whose future scope Xana cannot validate. If app-server does
not offer a one-effect acceptance, Xana declines the callback so the next
effect cannot bypass the child broker.

For native and managed children alike, the resolved child mode is also a hard
ceiling over matching configured rules: a `deny` child cannot be reopened by
an `allow` rule, and an `ask` child converts any matching `allow` rule to an
approval request. Rules may still narrow authority further.

## Scope and audit facts

File tools bind permission to the canonical target path beneath the canonical
launch workspace. `run_command` binds permission to the selected shell,
canonical working directory, and exact command string. Invalid arguments and
workspace escapes fail before policy evaluation. The immutable planned
arguments that receive permission are the arguments the concrete tool
executes.

Each result produces an audit fact containing its ids, tool and
effect, scope, final arguments, policy outcome, optional terminal decision,
and effective decision. The foreground runtime appends that fact as a distinct
session record before forwarding the audit event. Audit facts are never added
to model conversation. See [Sessions](sessions.md) for durability limits.

## Permission is not containment

An allow decision authorizes Xana to use the current Xana process's ordinary
host permissions. The broker does not create a sandbox, container, VM,
restricted token, filesystem jail, command classifier, or process timeout.
Canonical path checks prevent Xana's built-in file tools from accepting a
target outside the launch workspace, but they do not contain an allowed shell
command. Review command text and scope as host execution.
