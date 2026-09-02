# Commands and capability discovery

Xana keeps one repository-private typed command catalog for the CLI, plain
terminal, full-screen TUI, and Desktop. A frontend may choose different labels,
menus, shortcuts, or layout, but it does not define a second permission rule or
runtime handler.

Run the deterministic local report at any time:

```console
xana capabilities
xana capabilities --json
```

The report reads bounded local configuration and the current process/workspace
facts. It does not resolve credentials, contact a provider, start an integration,
or imply that a configured feature is reachable. This makes it safe and useful
when setup is missing, configuration is partially broken, or the machine is
offline.

## Facts that remain separate

Xana does not collapse these questions into a single `enabled` flag:

| Fact | Meaning |
|---|---|
| Availability | This build and surface implement the action and its prerequisites are present. |
| Selection | The current Profile or Conversation selected this connection, model, Skill, plugin, or route. |
| Authorization | This client currently has the observer, controller, or local-owner authority required by the action. |
| Permission | Runtime policy will deny, ask about, or allow the exact effect. It is still evaluated at dispatch. |
| Containment | Cooperative consent, supervised-process isolation, operating-system containment, and remote isolation are different guarantees. Xana never infers one from another. |
| Presentation | The surface can render or interact with color, Unicode, pointer input, clipboard, media, links, notifications, accessibility semantics, Markdown/math, or composed layout. Presentation never grants data or tool authority. |

An unavailable command remains discoverable when doing so is safe and includes
a stable reason such as `authority_required`, `setup_required`, or
`not_implemented`. Observer projections never advertise a mutating action as
enabled.

## Stable command semantics

Each action has a namespaced versioned ID such as `turn.submit.v1`,
`conversation.clear.v1`, `conversation.new.v1`, `conversation.preview.v1`,
`conversation.attach.v1`, `approval.decide.v1`, or `project.manage.v1`.
Frontend protocol commands carry the matching ID and reject a mismatched typed
payload. Results and documented failures also have stable codes; prose remains
owned by the presenting surface.

Argument shape, interaction requirements, authority, confirmation policy,
effect class, result code, and supported surfaces live with the same catalog
entry. Domain handlers still validate IDs, permissions, paths, credentials,
side effects, and durable state. The catalog cannot authorize an operation.

## Conversation wording and compatibility

`conversation` is the canonical family:

```console
xana conversation list
xana conversation new
xana conversation inspect SESSION_ID
```

The existing `xana session ...` CLI spelling and `/session` and `/sessions`
terminal spellings remain compatibility aliases. New documentation uses
`Conversation`; `Run` means one execution inside a Conversation.

`clear` and `new` remain different operations. Clear resets visible/model
context under the attached owner. New creates and navigates to a different
Conversation while retaining the prior one.

Preview and attach are also distinct. Preview reads bounded retained history
without acquiring control. Attach/resume acquires controller authority for the
composer. The current TUI supports explicit `/conversation preview ID`; the
same-surface attach command is discoverable but reports its current limitation
until Conversation switching lands. Selecting a normal Conversation row will
become attach/resume; preview will remain explicit.

User-facing Conversation states are `Attached here`, `Running`, `Needs input`,
`Idle`, `Preview only`, and `Archived`. Internal runtime ownership terms are not
instructions users must interpret.

## Presentation fallback

Plain output is the screen-reader and automation-safe baseline: bounded text,
no assumed pointer or clipboard, no inline media, and no composed layout. The
TUI advertises only detected terminal abilities; inline images remain false
without positive protocol detection. Desktop advertises native interaction and
rich rendering separately from runtime capability. Unknown future command IDs
are ignored safely rather than executed by resemblance.
