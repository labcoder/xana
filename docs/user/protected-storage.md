# Protected storage

> Audience: People using Xana's encrypted local content store.

Setup protects empty homes by default. Existing homes remain unchanged until an explicitly
reviewed migration. `xana storage status` inspects the format without
unlocking it. `xana doctor` distinguishes legacy, locked, unavailable-key and
invalid protected storage. No unavailable protected store falls back to plaintext.

You do not need to invent a password or create a security key. Xana generates
the keys and keeps normal unlock in your operating system's credential store.
This enables protected memory storage, not permission for extra provider calls.

## Start a fresh protected home

Start setup in an empty Xana home:

```text
xana setup
xana storage status
xana
```

For repository development, prefix commands with `cargo run --` and use the
platform's absolute path syntax. Set `XANA_HOME` to the intended isolated home
before these commands. Setup offers **Save recovery backup now** or **Later**.
Xana generates the backup; you only choose a new absolute file path outside the
managed data directory. The exported key is create-only and owner-accessible
(Unix 0600 or a protected Windows DACL).
Anyone who obtains it **and** the encrypted store can recover the content.
Do not put it in a chat, source repository, support bundle or shared folder.
This exports a recovery **key**, not a copy of your conversations; keep encrypted
data backups too (see [backups and reviewed restore](#backups-and-reviewed-restore)).

Normal startup unlocks through local Windows Credential Manager, macOS
Keychain, or Linux Secret Service, without authenticating every turn or file.
Unavailable custody is an error, not permission to write unencrypted data.
With **Later**, the recovery identity remains inside the encrypted database.
It is not an independent backup: losing the OS key can make the data
unrecoverable even if the encrypted files remain. Export later with:

```text
xana setup --section storage
# Or choose the exact destination directly:
xana storage recovery-export --output /absolute/separate/location/xana-recovery.txt
```

Export does not rotate the key, so it also recovers older encrypted snapshots.
`storage status`, Settings Overview and `doctor` show whether an export has been
recorded. They cannot verify that you retained the file or backed it up off-device.
Keys are never printed or automatically written as plaintext next to the database.

Desktop setup also protects empty homes, deferring the recovery export with a
visible warning. Use the terminal storage setup above for backup export or
existing-home migration; the Desktop Settings Overview shows protection status.
Noninteractive setup defaults to deferred export; pass `--recovery-output PATH`
to save it explicitly. `--legacy-storage` deliberately retains legacy behavior
for an empty terminal setup; it never downgrades a protected home. The deprecated
`init` command remains a configuration-only compatibility path.

For advanced/manual custody, existing explicit-key commands remain supported:

```text
xana storage recovery-key --output /absolute/separate/location/xana-recovery.txt
xana storage initialize --recovery-key /absolute/separate/location/xana-recovery.txt --manual-unlock
```

`--manual-unlock` deliberately skips OS custody; for each launch set
`XANA_STORAGE_RECOVERY_KEY` to the independent key **file path**.
That variable explicitly selects recovery-file unlock, not a fallback or an
inline secret. It does not bypass an explicit lock.

### Isolated development test (PowerShell)

Open a separate PowerShell window in the Xana checkout. These environment
changes apply only to that window; closing it returns you to your usual home.
Each run chooses a new test directory outside the repository:

```powershell
$xanaTestRoot = Join-Path $env:LOCALAPPDATA ('XanaTests/' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $xanaTestRoot | Out-Null
$env:XANA_HOME = Join-Path $xanaTestRoot 'home'
Remove-Item Env:XANA_STORAGE_RECOVERY_KEY -ErrorAction SilentlyContinue
cargo run -- setup
cargo run -- storage status
cargo run
```

Keep that window and the test paths to restart the same test home later. Setup
requires a connection again; an isolated Xana home does not isolate third-party
accounts such as Codex login. Ollama with an already downloaded model is a useful
local test route. Do not reuse or overwrite your normal recovery key.

To test fresh setup **without** protected storage instead, choose another
new test root/home, then run `cargo run -- setup --legacy-storage`.
Chat works, but personal
memory remains unavailable. It should not require migration merely to chat.

Suggested memory checks: ask for an unknown name (no save), explicitly remember
a preference, quit/reopen the same Conversation and recall it, then create a new
Conversation and verify the narrow memory does not carry across. Explicitly ask
for a user-wide preference, approve the scope review, and verify cross-Conversation
recall. These checks test model behavior as well as storage; inspect committed
records with `/memory list` if the model gives an unexpected answer.

## Unlock and recovery

```text
xana storage verify
xana storage lock
xana storage unlock
xana storage unlock --recovery-key /absolute/separate/location/xana-recovery.txt
xana storage unlock --recovery-key /absolute/separate/location/xana-recovery.txt --remember
```

`--remember` restores local OS custody after verified independent recovery.
Without it, subsequent opens still need custody or the explicit recovery-file
environment setting. A lost OS credential is recoverable only with a separately
saved recovery key; losing both custody and the recovery key is not.

## Locking and active work

In terminal chat, `/storage lock` requests the normal bounded shutdown first,
retains interruption/uncertain-work receipts, drops the TUI and drafts, then
locks and exits. Accepted interrupted work remains recoverable, never silently
replayed. Desktop's **Stop work and lock storage** command replaces the entire
Workbench with a content-free privacy screen before attempting the lock.

Other live Xana clients must close too. The final exclusive home lease refuses
to report Locked while another key owner remains. A failed lock closes the
requesting storage handle but does not falsely claim the whole home is locked.
An explicit unlock creates new handles; it cannot revive revoked ones.

## What is protected

SQLCipher encrypts native Conversation records, their ancestry/catalog indexes,
private Project and package records, managed Codex handle metadata, workspace
descriptors and composer history. Native IDs and artifact references are unchanged.
Standard age envelopes encrypt immutable artifact objects and the recovery bundle.
Ranges authenticate the entire object while retaining only the requested bytes;
this is bounded-memory streaming, not constant-time random access.

Normal previews and provider image input decrypt in memory. Codex receives
validated image data URLs, not temporary plaintext image files. To open an
artifact in another application, explicitly **Save/export** it first to a
destination you choose. An existing destination is never overwritten; a failed
verification removes only the export file created by that attempt.

Ordinary source files and non-secret configuration/presentation preferences
remain editable normally. Provider credentials keep their existing owner.
Metadata-only diagnostics, opaque IDs, file sizes, timestamps and access patterns
are not hidden. Third-party installed plugin code is not personal memory.
Vendor/provider stores, OS swap/crash dumps, filesystem snapshots, clipboard,
terminal scrollback and deliberate plaintext exports are separate boundaries.
Encryption does not secure a compromised unlocked account or unsend model context.

Protected session reset removes the selected logical records, not the database
or recovery envelope. Unreferenced ciphertext may remain; this is not secure
erasure. Incomplete initialization fails closed and retains encrypted remnants
for review. Do not remove the format marker to force an old binary to open a
protected home. New configuration uses schema version 5; older configurations
remain readable without automatic mutation.

## Migrate an existing home

Close every Xana client/host using that home. First finish any pending
`xana config migrate --apply` transaction and resolve invalid private records
reported by doctor. The guided flow previews the inventory and asks for approval
before converting anything. Choose Save recovery backup now or Later; no
user-created key is required:

```text
xana setup --section storage
xana storage verify
```

For your normal home from this checkout, close Xana and run
`cargo run -- setup --section storage`, then `cargo run -- storage status`.
Restart Xana afterward so the conversation attaches to protected storage.

The scriptable path remains available:

```text
xana storage migrate
xana storage migrate --apply --review DIGEST_FROM_PREVIEW
xana storage verify
```

The preview hashes the config and bounded inventory; changed source files
invalidate approval. Conversion preserves Conversation/record IDs, managed
handles, project state, artifact references and original recency. Config schema
5 fences old binaries. A restart journal and exclusive writer checks prevent
competing writes while the verified encrypted generation is activated.
Pass `--recovery-key PATH` to retain an existing user-owned-key workflow; add
`--manual-unlock` to deliberately select recovery-file rather than OS custody.

After interruption, reopen `xana setup --section storage` or resume directly.
Automatically managed migrations recover their original key through OS custody:

```text
xana storage migrate --resume
```

For a migration started with a user-supplied key, resume with that same
`--recovery-key PATH`; Xana never invents a replacement key.
The command reports the retained `data.legacy.UUID` plaintext generation.
**It is not deleted automatically and is not encrypted retroactively.** Check
recovery and your records before deciding whether to remove it. Secure SSD
erasure is not promised. An unexpected changed source/destination stops recovery
with both generations retained; do not remove the journal to force startup.

Unknown derived files and torn journal tails are kept as encrypted archives,
not installed as live plaintext files. Inspect/export them explicitly:

```text
xana storage archive
xana storage archive --after LAST_ID
xana storage archive --export ARCHIVE_ID --output /chosen/new-file
```

## Backups and reviewed restore

```text
xana storage backup-policy
xana storage backup-policy --retention-days 7 --max-snapshots 7 --max-bytes 1073741824
xana storage backup
xana storage backup --if-due
```

Defaults are seven days, seven snapshots, one GiB, and a 24-hour due interval.
The default directory is a `data.backups` sibling of the data directory; set an
absolute `--directory` on `backup-policy` to choose another disk. `--enabled
false` skips due-driven maintenance, not an explicitly requested backup.
These commands perform maintenance when invoked; they do not install a timer,
OS service or remote synchronization. A detached scheduler is separate work.

Snapshots use the encrypted-to-encrypted SQLite backup API and copy immutable
age objects, then verify database pages, history/references and complete objects.
Only a verified replacement permits bounded pruning. Unrecognized files or a
live snapshot reader prevent cleanup. If size/space prevents a fresh copy, the
last usable snapshot remains and missing coverage is reported. No canonical
Conversation history expires because of this policy. Interrupted `.pending`
generations are not usable snapshots or silently counted as coverage.

```text
xana storage restore --snapshot /chosen/SNAPSHOT_UUID --recovery-key /separate/recovery.key
xana storage restore --snapshot /chosen/SNAPSHOT_UUID --recovery-key /separate/recovery.key --apply --review DIGEST_FROM_PREVIEW
xana storage restore --resume --recovery-key /separate/recovery.key
```

Restore requires a protected or empty destination home and all owners stopped.
It replaces only protected content; ordinary sources, configuration, installed
plugin code and inert preferences stay in place. The prior encrypted generation
is retained. It does not replay model/tool operations or reinstate background
authority. Recall, learning and automation require a later review of current
forgetting exclusions and grant/job state; restoring older bytes is not consent
to use them. Rebind OS custody explicitly with `storage unlock ... --remember`
when recovering on another machine. Backups do not include vendor credentials,
ordinary configuration or source files; maintain those separately as needed.
