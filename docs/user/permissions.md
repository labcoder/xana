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
Current tool names are `read_file`, `list_files`, `edit_file`, and
`run_command`. Workspace matchers are relative to Xana's launch workspace;
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
session grant cannot override a matching deny rule or a default deny. Grants
exist only in process memory and disappear when Xana exits.

Blank input, EOF, an unknown or stale decision, controller loss, and an
unattended ask all deny. A pending request is correlated by operation and tool
invocation ids. `allow once` never authorizes a later invocation.

## Scope and audit facts

File tools bind permission to the canonical target path beneath the canonical
launch workspace. `run_command` binds permission to the selected shell,
canonical working directory, and exact command string. Invalid arguments and
workspace escapes fail before policy evaluation. The immutable planned
arguments that receive permission are the arguments the concrete tool
executes.

Each result produces an in-memory audit fact containing its ids, tool and
effect, scope, final arguments, policy outcome, optional terminal decision,
and effective decision. Audit facts are runtime observations; they are not
added to model conversation or persisted in this version.

## Permission is not containment

An allow decision authorizes Xana to use the current Xana process's ordinary
host permissions. The broker does not create a sandbox, container, VM,
restricted token, filesystem jail, command classifier, or process timeout.
Canonical path checks prevent Xana's built-in file tools from accepting a
target outside the launch workspace, but they do not contain an allowed shell
command. Review command text and scope as host execution.
