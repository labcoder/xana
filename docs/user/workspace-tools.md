# Workspace file, search, and command tools

> Audience: People installing, configuring, or using Xana.

Xana's native agent uses a small typed tool set for ordinary repository work.
These tools share the same permission broker, durable operation records, path
checks, and immutable per-turn schema. Managed Codex owns a different inner
tool loop; the contracts below apply only when Xana is the execution owner.

Personal facts use separate [memory tools](personal-memory.md), not workspace
files. `memory_lookup`, `memory_remember`, `memory_correct` and `memory_forget` share Xana's protected memory policy
and are also exposed through its narrow managed Codex bridge.

## Tool contracts

| Tool | Purpose | Important bounds | Replay declaration |
|---|---|---|---|
| `read_file` | Read UTF-8 text by inclusive line range or byte page | At most 64 KiB per result | `Safe` |
| `list_files` | List one directory without recursion | 256 entries and 64 KiB output | `Safe` |
| `find_files` | Discover workspace paths with one glob | Depth 32, 1,000 results, 50,000 visited entries, 64 KiB output, two seconds | `Safe` |
| `grep_files` | Search bounded UTF-8 files with literal text or a bounded Rust regex | 2 MiB per file, 16 MiB total input, 1,000 matches, 64 KiB output | `Safe` |
| `write_file` | Explicitly create a missing UTF-8 file or atomically overwrite an existing one | 256 KiB content | `Never` |
| `edit_file` | Atomically apply exact replacements against the original UTF-8 bytes | 64 KiB file/result, 32 edit specifications | `Never` |
| `run_command` | Run one command through the configured shell | 30-second default, 120-second ceiling, 32 KiB retained independently for stdout and stderr | `Never` |
| `web_fetch` | Retrieve one exactly reviewed public HTTPS text resource | Three reviewed redirects, 1 MiB default/4 MiB maximum response, 20-second default/60-second maximum timeout, 24 KiB inline text | `Safe` |
| `read_document` | Extract bounded text from a supported document | Format-specific input and output limits | `Safe` |
| `xana_docs` | Read Xana's compiled, version-matched documentation | 32 KiB per read | `Safe` |

`Safe` means an unfinished invocation may be eligible for one explicit,
reauthorized recovery replay. It does not mean the tool bypasses permission or
that Xana silently retries it. `Never` means an unknown outcome is recorded
and never repeated automatically.

## Discover, read, and search

Use `find_files` for recursive path discovery and `grep_files` for content
search. Both walk in stable path order, stay inside the launch workspace, do
not follow symlinks, and honor local ignore rules without consulting a user's
global Git ignore. A search rooted below the workspace still applies the
workspace-root `.gitignore`; nested ignore files continue to apply as the walk
descends.

Discovery and search have no stable cursor because the filesystem can change
between calls. When a result reports `truncated`, it also reports which bound
was reached and instructs the agent to rerun with a narrower path, glob,
query, or result count. Xana does not retain a hidden complete result or put it
in the prompt. Binary files, invalid UTF-8, oversized inputs, changed paths,
and traversal errors are counted explicitly instead of being presented as
successful matches.

`read_file` keeps its line-range form for small targeted reads. For a large
file, pass `offset_bytes` and `max_bytes`; the JSON result includes the exact
`next_offset_bytes`, total byte length, and truncation fact. Offsets are
zero-based UTF-8 boundaries. `offset_bytes` cannot be combined with line ranges;
`max_bytes` can also cap a line-range read. It is a capacity of 4–65,536 bytes,
not the file's size: a three-byte file can use a 4,096-byte capacity or just
`{"path":"notes.txt"}`. Line reads enforce the cap while reading chunks, even
when a file contains a very long line.

## When tools make no progress

Xana stops a native turn after three matching failed calls in its recent
32-failure window, or six consecutive tool errors. This includes invalid
arguments rejected before execution. Continuation does not reset the guard;
a new message does. Unexecuted calls in a committed batch receive error results
so history remains consistent. Correct the reported error before retrying.
Successful reads of different pages and successful polling are not blocked by
this failure guard. A failure is not presented as a completed answer.

## Create and edit

`write_file` always requires `mode = "create"` or `mode = "overwrite"`.
Create refuses an existing path. Overwrite refuses a missing or non-file path,
stages the complete replacement beside the target, preserves its permissions,
and commits with an atomic same-filesystem rename.

`edit_file` retains the simple `old_text`/`new_text` form and also accepts up
to 32 exact edit specifications. Every specification declares the expected
match count and may select one one-based occurrence or replace all confirmed
occurrences. Xana computes every span against the original bytes and rejects
overlap, count mismatch, invalid UTF-8, changed identity/content, or an
oversized result before committing one atomic replacement. It is deliberately
not a fuzzy rewrite or patch language.

Workspace-relative paths are the normal form. `read_file`, `write_file`, and
`edit_file` may plan one absolute file outside the workspace. That exact
canonical path uses the existing external-path approval flow; it does not
grant a directory or make discovery/search leave the workspace. Parent
directories for newly created files must already exist.

## Commands and timeouts

`run_command` receives one command, one existing working directory inside the
workspace, and an optional `timeout_ms`. The default is 30,000 ms and the
immutable ceiling is 120,000 ms. A timeout stops the owned child process and
returns a typed failure. Retained stdout and stderr are bounded independently
and each reports truncation. The command still runs with Xana's ordinary host
permissions after authorization; this is not a sandbox.

The shell kind and executable are configured at setup time. Tool output and
timeout ceilings are intentionally not configuration settings, so a model,
project instruction, or profile cannot silently widen them.

## Troubleshooting

| Symptom | Meaning and response |
|---|---|
| `path ... is unavailable` | The path does not exist at planning/execution time, an ancestor is missing, or it changed. Search from a known workspace directory and read again. |
| `resolves outside the workspace` | A workspace-only tool encountered an escaping path or symlink. Use an exact absolute path only with the file tools that support external review. |
| `truncated: true` | A declared count, byte, entry, or time limit was reached. Narrow the next request; there is no hidden overflow result. |
| binary or invalid UTF-8 skipped | `grep_files` searches text, not arbitrary bytes. Use an appropriate document/media service instead. |
| edit match-count or overlap failure | Re-read/search the current file and submit unique non-overlapping exact replacements. No partial edit was committed. |
| command timed out | Increase the per-call timeout only if the operation is understood, up to 120 seconds, or run a narrower command. |

Permission prompts and policy configuration are documented in
[Permissions](permissions.md). Interrupted-operation inspection and replay
rules are documented in [Operation recovery](operations.md).
Public network retrieval has a separate exact-recipient and outbound-data
boundary documented in [Native web fetch](web-fetch.md).
