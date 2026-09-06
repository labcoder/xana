# Detached host and durable schedules

Xana can keep explicitly authorized local work after every client closes. The
detached host is off by default. Creating a schedule does not start it, closing
an observer does not stop it, and enabling it does not install an OS startup
entry. Jobs, authority, budget ceilings, and completion receipts live in protected
storage; there is no plaintext fallback when keys are unavailable.

## Create and inspect work

Use `xana autonomy --help` and `xana autonomy create --help` for the complete
typed controls. Inspect your global Profile and connection first. Each saved
task binds the exact workspace identity, optional Project, Profile identity,
connection, model, endpoint, permission configuration, expiry, and fixed action.
It has its own Conversation, not the Conversation focused in a client.

For example, substitute your actual workspace, native Profile, and future dates:

```text
xana profile inspect YOUR_PROFILE
xana autonomy create --name "Review notes" --workspace "C:/work/project" --profile YOUR_PROFILE --reminder "Review the project notes" --at "2026-10-01T09:00:00-07:00" --expires "2026-10-02T09:00:00-07:00" --authorize
xana autonomy list
xana autonomy show JOB_ID
```

A reminder completes by writing a durable local receipt. It does not send mail,
invoke a model, or promise a system notification. It still uses the explicitly
selected scope and Profile, which must remain available at wake.

A native task can use the Profile's already permitted bounded workspace reads:

```text
xana autonomy create --name "Daily notes summary" --workspace "C:/work/project" --profile YOUR_PROFILE --prompt "Summarize note.txt and identify open questions" --workspace-reads --daily "09:00" --timezone "America/Los_Angeles" --expires "2026-11-01T00:00:00-07:00" --authorize
```

`--workspace-reads` permits only the intersection of the Profile's capabilities
with `read_file`, `list_files`, `find_files`, `grep_files`, `read_document`, and
`xana_docs`. Existing deny/ask rules still apply. External paths, writes, commands,
plugins, MCP, child agents, and external effects are not available to these
tasks. Prompt assembly includes normal bounded workspace instructions and
eligible personal memory under the task's exact Conversation/Profile/Project
scope. Task outputs do not automatically create learned memory proposals.
While idle, the enabled host can drain already queued personal-learning work
through your separately configured learning helper. That worker uses the same
shared background lane, source/privacy checks, cancellation and usage limits;
enabling the host does not grant a learning-helper route or enqueue task output.

The Profile must allow prompt disclosure and, when reads are selected, selected
file-content disclosure. No client is assumed to be present: a new approval
question fails closed and the task enters `NeedsYou`. Connecting an observer
does not grant permissions or continue the task. Profiles requiring managed
Codex execution, explicit reasoning options, identity layers, or skill layers
are rejected by this initial detached execution path.

Terminal and TUI chat expose the same owner operations as `/autonomy ...`;
their command palette opens the bounded overview. Long-running `host run` and
`host observe` belong in a dedicated CLI process. Desktop's **Schedules** panel
provides the same creation, inspection, pause/resume/cancel, and host controls.
Desktop creation requires an exact preview and confirmation; changing the draft
or resolved route invalidates that preview. Disk and configuration work happens
outside the UI thread. Refresh after edits to obtain the new revision.

## Selected files and named CI runs

Calendar time is not the only trigger. `--watch-root` and `--github-run` replace
`--at`/`--daily`; the action, Profile, recipient, expiry and budget still need
explicit authorization. These commands save intent, not a running watcher:

```text
xana autonomy create --name "Notes changed" --workspace "C:/work/project" --profile YOUR_PROFILE --watch-root notes --reminder "Review the changed notes" --expires "2026-11-01T00:00:00-07:00" --authorize
xana autonomy create --name "Named CI run" --workspace "C:/work/project" --profile YOUR_PROFILE --github-run OWNER/REPO/RUN_ID --github-credential env:XANA_GITHUB_TOKEN --reminder "Review the named CI run status" --expires "2026-11-01T00:00:00-07:00" --authorize
```

Use a fine-grained GitHub credential with Actions read access to the named
repository. The explicit reference can be `env:NAME` or `stored:ID`; it is not
the token itself. Xana does not borrow `gh` authentication, enumerate your
accounts, download CI logs, rerun jobs or modify the repository. The adapter
uses GitHub's [versioned REST API](https://docs.github.com/en/rest/about-the-rest-api/api-versions).
Repeated unchanged statuses do not call a model. A rerun or changed commit
requires new reviewed intent rather than silently changing the resource.

The file watcher checks a selected workspace directory at five-second
intervals, with a stable sample across at least two seconds before admission.
It checks at most 256 entries and 16 directory levels, refuses links/reparse
points and special files, and rejects overlap with Xana-managed state. Overflow,
root replacement, or uncertain shell effects requires owner review. Select a
small source folder, not an entire repository containing build output.

This is a coalesced metadata watcher, not a filesystem audit trail. A change
reverted between polls, or content changed while deliberately preserving size
and timestamps, may not be observed. Successful built-in writes record their
observed output revision for loop suppression; later differing revisions are
eligible changes. Unknown shell outputs cannot be treated as safely attributed.
Watching never turns file contents into instructions, and unattended actions
retain their read-only ceiling.

GitHub polling normally waits 60 seconds, uses conditional requests, respects
server retry windows, and has bounded backoff. Transport/resource/credential
failures are distinct from an unchanged run. No alternative credential is tried.
The last status and pending event are retained across restart; terminal run
completion ends the named-run task after its authorized action.

## Review upcoming and background work

`xana autonomy overview` (or `/autonomy`) reads a bounded page of durable work,
including Coming up, In motion, Paused, Needs you and terminal outcomes.
`xana autonomy review JOB_ID` shows the saved scope, watched source, last check,
last event, pending action, current route agreement, budget ceilings and scoped
memory controls. A ceiling is not actual usage; use the usage ledger for charges.
`list` and `show` remain available for full local structured inspection.

Desktop's Espejo **Coming up and background work** section reads these same
records, with Global/Project filtering and bounded pagination. Opening a row
opens exact review in **Schedules**; it never acquires a Conversation controller
or grants new authority. Refresh is explicit. A changed route requires a newly
reviewed task, not an invisible grant expansion.

While Desktop is attached, a passive bounded observer also reports new
`NeedsYou` and completion receipts. It establishes a quiet baseline at connect,
deduplicates task/revision events, and does not call a model or claim control.
In-app attention remains available when the window is focused; OS notifications
follow your notification policy and focus state. Opening a notice leads to
review in Schedules. This attention feed is not an automatically refreshed
full work list: use **Refresh work** for current row state.

No-memory disables personal use/learning at the selected scope; it does not
erase encrypted task history or execution receipts. Background execution cannot
invent an attached human approver. `NeedsYou` remains visible until the owner
reviews the cause and takes an explicit revision-checked action.

## Start, detach, stop, and lock

```text
xana autonomy host status
xana autonomy host start --revision POLICY_REVISION
xana autonomy host observe
```

Start requests a separate hidden local process. Status and observation confirm
its readiness; a successful spawn alone is not a ready receipt. All clients
share the existing authenticated loopback discovery mechanism. A competing
launch cannot acquire the same-home host lease or recover the owner's live
occurrence. Observation is passive and bounded to the first 32 job summaries;
use paginated owner inspection for the full retained set. No prompt bodies or
credentials are published in those summaries.

On Windows the detached owner does not inherit its launcher's open handles,
including redirected command-output pipes and private file leases. Capturing
`host start` output therefore finishes independently of the host's lifetime;
the explicit home and recovery-file-path environment remain available to the
child when manual unlock was selected.

Ctrl+C in `host observe`, closing a terminal observer, or closing Desktop only
detaches that client. These are different operations:

```text
xana autonomy host stop --revision POLICY_REVISION
xana autonomy host stop --revision POLICY_REVISION --lock
xana autonomy host disable --revision POLICY_REVISION
```

Stop prevents new admission and requests cancellation of the active task.
Disable also revokes future detached launches and startup permission. Stop with
`--lock` requests key sealing after owned work and observation have shut down;
other live protected-store owners can prevent that final lock. A request is not
a stop or lock acknowledgement. Check host status and protected-storage status.
Jobs and receipts are retained. A later start requires a fresh policy revision.

The stop receipt names the active job and its task Conversation from the same
transaction that stops admission, even when that job is beyond the first list
page. When the owning host observes that policy revision, it records the
connected client identities/count and host generation. Inspect `host status`
or Desktop's refreshed host status for this `stop_impact`. An absent client
snapshot means unacknowledged, not zero clients. The snapshot names clients
connected at acknowledgement; it is not an exhaustive history of past clients
or a promise that no one reconnects while work drains. All queued schedules
stop admitting; per-job terminal receipts still determine what actually ran.

Normal native shutdown waits up to eight seconds for acknowledgement, then
aborts and joins the runtime's owned worker before releasing the background and
workspace leases. It does not kill unrelated processes. Local interruption
cannot prove that a remote provider stopped computing or charging.

## Explicit OS startup

`xana autonomy host startup --revision POLICY_REVISION --enable` explicitly
installs this home's per-user login entry, then grants startup permission. Omit
`--enable` to revoke that permission and remove the matching entry. Desktop
labels these actions **Install login startup** and **Remove login startup**.
Neither operation launches a host immediately.

Windows uses a home-specific value in [HKCU Run](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys),
macOS uses a per-user [LaunchAgent](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
and Linux uses [XDG desktop autostart](https://specifications.freedesktop.org/autostart/0.5/).
No machine-wide service, elevation, shell script, or automatic restart loop is
installed. Linux needs a desktop session implementing XDG autostart; this is
not a headless system service. Windows rejects commands beyond the documented
260-character Run limit. Linux rejects paths with unsupported desktop-entry
field-code characters instead of interpreting them.

The entry contains only the exact CLI path and typed startup/home arguments,
not task text or credential values. An explicit home is preserved with
`--home`; default-location startup ignores an inherited `XANA_HOME`. Removing
an entry checks the saved command, preserving a differing replacement. Changed
executable locations require re-enabling startup to register the new path.

OS registration and protected policy cannot be one transaction. Xana revokes
startup first and grants it only after a successful installation; an interrupted
edit can leave an inert registration. Refresh policy after an error. Disabling
the whole detached host revokes startup authority; use the startup removal
control as well if you want its inert login entry removed.

Startup succeeds only after login with available protected keys, detached
permission, startup permission, and no persisted stop request. There is no
pre-login unlock or always-on availability promise. Desktop needs the `xana`
CLI executable beside `xana-desktop` to request a detached launch.

## Clock, budget, and recovery rules

- One-shot times are RFC3339 instants with an offset. Daily times use an explicit
  bundled IANA timezone; changing the machine's timezone does not change them.
- A nonexistent daily time runs at the first valid instant after the gap
  (02:30 becomes 03:00, not 03:30). A repeated time uses its earlier occurrence
  only. The receipt records gap adjustment.
- Sleep or downtime coalesces missed triggers into one saved occurrence.
  After completion, the next occurrence is calculated from the current date;
  Xana does not replay every missed day. A backward clock does not duplicate a
  committed occurrence.
- One shared protected-home background lane runs at a time. Foreground activity
  takes priority and cancels background work. A busy workspace is deferred for
  30 seconds before dispatch. There is no automatic retry after a started run
  has an unknown outcome.
- A saved task records ceilings of 8,192 conservative input/output tokens and
  120 seconds per run; all background work shares 32,768 tokens per UTC day.
  Lower configured usage limits also apply. Admission reserves spend in the
  existing durable ledger before provider dispatch. Uncertain reservations
  are not refunded merely because a client disconnects.
- The queue admits at most 1,000 active jobs. List and receipt reads are cursor
  bounded to 32 records, task text to 16 KiB, and protected job records to 64 KiB.
  The native loop allows at most four tool rounds or the Profile's lower limit.

These are conservative local admission limits, not an exact vendor tokenizer
or a promise that remote billing ends at a cancellation deadline. Inspect the
usage ledger for known and uncertain charges.

```text
xana autonomy receipts JOB_ID
xana autonomy pause JOB_ID --revision JOB_REVISION
xana autonomy resume JOB_ID --revision JOB_REVISION
xana autonomy cancel JOB_ID --revision JOB_REVISION
```

Pause prevents new runs but lets an in-flight run finish. Cancel revokes future
authority and requests interruption of an active run; its terminal receipt can
still report completed or uncertain work. Expiry stops new admission and
requests cancellation at the next active policy check.

On restart, a claimed occurrence without a terminal receipt becomes `Unknown`
and `NeedsYou`, never an automatic replay. Inspect its receipt, task-owned
Conversation, and usage first. For authority that has not been cancelled or
expired, `resume --review-unknown` explicitly authorizes a new attempt with a
new occurrence identity while retaining the uncertain receipt. Changed scope,
Profile, endpoint, or permission policy requires a newly reviewed task. Restored
unattended authority is blocked by protected-storage restore review.

See [usage budgets](usage-budgets.md), [personal memory](personal-memory.md), and
[protected storage](../architecture/protected-storage.md) for related controls.
