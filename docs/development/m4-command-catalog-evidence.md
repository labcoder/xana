# M4 typed command and presentation-capability evidence

M4-05 replaces frontend-local command descriptions with one application-owned
catalog while retaining existing runtime handlers as the authority boundary.

## Implemented evidence

- `command_catalog` owns stable namespaced IDs, aliases, argument shapes,
  observer/controller/owner requirements, interactive behavior, confirmation,
  effects, supported surfaces, result/error codes, and availability reasons.
- `conversation` is canonical. CLI `session` and terminal `/session` and
  `/sessions` remain compatibility aliases.
- `conversation.clear.v1`, `conversation.new.v1`,
  `conversation.preview.v1`, and `conversation.attach.v1` are distinct. The
  unfinished TUI attach projection is visible but disabled; it is not silently
  treated as preview.
- TUI slash parsing and the command palette use the shared catalog. Plain chat
  parsing and help use the same projection. Clap's visible top-level command
  families are checked against the catalog.
- Desktop receives a presentation-safe authority-filtered catalog projection
  for later menus, buttons, and shortcuts. An observer cannot receive an
  enabled mutating descriptor.
- Frontend protocol version 5 carries a semantic command ID alongside the typed
  value and rejects mismatches before dispatch. Accepted/rejected results carry
  stable semantic outcome codes.
- `xana capabilities [--json]` reports platform, host, workspace permission,
  Profile, connection/model selection, extension selection, containment model,
  presentation profiles, command availability, and missing setup without
  credential or network probes.
- Presentation capability profiles cover color, Unicode, dimensions, pointer,
  clipboard, inline media, safe links, notifications, accessibility, rich
  Markdown/math, and layout composition without granting runtime authority.

## Verification

The focused tests cover:

- catalog ID uniqueness and migration aliases;
- clear/new and preview/attach distinctions;
- observer and noninteractive fail-closed projections;
- rich, limited, plain, and hostile-terminal presentation fallbacks;
- safe handling of one synthetic future command;
- CLI, TUI, plain, Desktop, and frontend-protocol conformance;
- deterministic missing/valid configuration capability reports and credential
  redaction.

The repository-wide formatting, strict Clippy, all-feature test, and
no-default-feature test gates are required before completion.
