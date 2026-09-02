# Logs and crash diagnostics

> Audience: People diagnosing a local Xana installation or preparing a support report.

Xana writes structured, metadata-only process logs under the resolved data
directory. With `XANA_HOME`, the defaults are `XANA_HOME/data/logs` and
`XANA_HOME/data/crashes`; platform installs use their ordinary Xana data
directory. Nothing is uploaded and logs are not a transcript.

```console
xana logs path
xana logs list
xana logs show xana-TIMESTAMP-PID.jsonl --lines 200
xana logs show xana-TIMESTAMP-PID.jsonl --follow
xana logs export --output C:\absolute\path\xana-support.json
```

Use an absolute POSIX output path on macOS or Linux. `list` shows at most 256
regular Xana-owned entries. `show` accepts one exact listed filename, reparses
known structured records, omits malformed/oversized lines, and never follows a
symlink. `--follow` is a foreground local tail stopped with Ctrl+C. `export`
creates a new file, never overwrites one, reparses only known record schemas,
runs a second secret-shaped scan, and produces at most 8 MiB. Review the JSON
before sharing it.

## Configuration and bounds

```toml
[diagnostics]
enabled = true
level = "info"
targets = ["application", "runtime", "provider", "tool", "frontend", "storage", "integration", "security"]
# directory = "diagnostic-logs" # relative to Xana's data root, or absolute
retention_days = 7
max_file_bytes = 4194304
max_total_bytes = 33554432
max_files = 32
queue_capacity = 1024
```

Levels are `error`, `warn`, `info`, `debug`, and `trace`; verbosity never
weakens redaction. Targets select stable ownership areas, not Rust module names.
Configuration is loaded at process start. Bounds are mandatory: retention is
1–365 days, one file is 64 KiB–64 MiB, total storage is no more than 512 MiB,
file count is 1–256, and the nonblocking queue is 64–8192 events. The default
keeps seven days within the file/count/byte ceilings.

Log directories are private where the platform exposes portable owner modes.
Xana rejects relative traversal and symlink components, creates files with
exclusive create semantics, and cleans only recognized regular `.jsonl` logs
inside the configured log root. Old malformed files cannot prevent startup;
sink faults degrade diagnostics rather than runtime authority or execution.
On ordinary shutdown Xana requests a writer drain and waits for its bounded
acknowledgement before joining the writer; an unresponsive sink is detached at
the deadline rather than hanging process exit.

Desktop notification preferences are separate from diagnostic retention:

```toml
[notifications]
enabled = true
approvals = true
questions = true
completions = true
failures = true
controller_lost = true
host_failures = true
```

These switches permit only fixed metadata-only attention messages while Xana
is unfocused or minimized. Notification text never contains prompts, answers,
reasoning, paths, tool arguments, or secrets. Routine streaming and tool
activity do not create native notifications. The in-app Conversation, Activity,
and Diagnostics projections remain authoritative if delivery is suppressed,
deduplicated, delayed, or unavailable.

## What is and is not recorded

Records contain a version, timestamp, level, target, typed event/outcome,
process and sequence numbers, optional safe or hashed identifiers, duration,
size, and dropped-event count. A bounded nonblocking writer prevents logging
from stalling the agent. Queue pressure increments a loss counter carried by a
later record and reported by Doctor.

No level records credentials, authorization/OAuth material, environment
values, prompt or response bodies, hidden reasoning, file/clipboard contents,
raw paths or URLs, tool arguments/results, or artifact bytes. Session journals,
permission audit facts, and operation recovery remain separate authoritative
records; logs do not duplicate them.
Prompt-plan ledgers and compaction summaries are likewise excluded from log
files. Their bounded metadata is available through the attached frontend and
`xana session inspect`; the append-only session checkpoint remains authoritative.
The separate `data/interoperable/outbound-audit.json` journal contains only
bounded recipient/class/count/digest metadata and keeps at most 512 records.
Its pre-send facts are authoritative; diagnostic forwarding remains
observational and follows the log settings above.

## Crashes and unclean exits

Before writing an in-process crash report, Xana makes a best-effort terminal
restore. A report contains platform/version facts, a typed panic/task exit,
hashed panic location and backtrace identity, and at most 64 metadata-only
breadcrumbs. Panic text is excluded. A locked per-process run marker is removed
on clean shutdown. A later process can distinguish a currently locked marker
from a stale prior marker and points to `xana logs list` and `xana doctor`.

An OS kill, power loss, or process abort may leave only the unclean marker; Xana
does not claim it can always write an in-process report. Raw memory dumps,
telemetry, automatic uploads, and hosted crash reporting are not supported.

On the next ordinary mutable launch, Xana also performs bounded conservative
artifact reconciliation. It removes only unlocked regular staging files with
Xana's exact `.UUID.tmp` name under `data/artifacts`. It preserves active locked
writers, published content-addressed artifacts, symlinks, and unrelated files.
This cleanup is idempotent and never resumes an interrupted Run, reuses a stale
approval/controller, or calls a provider or tool. Failure is logged as a typed
storage recovery fact and remains visible instead of triggering destructive
repair.

`xana doctor` inspects configured roots, path safety, portable write-permission
metadata, owner-only permissions, file/count/byte/age retention compliance,
record validity, stale markers, writer faults, and observed event loss without
creating or deleting diagnostic state. Normal Xana execution starts the writer;
`doctor` and `logs` inspection commands remain read-only.
Because this check creates no probe file, it cannot prove free space or every
platform ACL; a later writer fault remains a separate visible health signal.
