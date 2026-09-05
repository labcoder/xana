# Protected storage

> Audience: People using Xana's encrypted local content store.

Protected storage is opt-in. Existing homes remain unchanged and are **not**
retroactively encrypted. `xana storage status` inspects the format without
unlocking it. `xana doctor` distinguishes legacy, locked, unavailable-key and
invalid protected storage. No unavailable protected store falls back to plaintext.

## Start a fresh protected home

First choose an empty isolated Xana home. Keep the independent recovery key
outside its managed data directory, preferably backed up on another medium:

```text
xana storage recovery-key --output /absolute/separate/location/xana-recovery.txt
xana storage initialize --recovery-key /absolute/separate/location/xana-recovery.txt
xana init
xana
```

For repository development, prefix commands with `cargo run --` and use the
platform's absolute path syntax. Set `XANA_HOME` to the intended isolated home
before these commands. Initialization refuses existing data. The recovery-key
file is create-only and owner-accessible (Unix 0600 or a protected Windows DACL).
Anyone who obtains it **and** the encrypted store can recover the content.
Do not put it in a chat, source repository, support bundle or shared folder.

Normal startup unlocks through local Windows Credential Manager, macOS
Keychain, or Linux Secret Service, without authenticating every turn or file.
Unavailable custody is an error, not permission to write unencrypted data.
`--manual-unlock` on initialization deliberately skips OS custody; for each
launch set `XANA_STORAGE_RECOVERY_KEY` to the independent key **file path**.
That variable explicitly selects recovery-file unlock, not a fallback or an
inline secret. It does not bypass an explicit lock.

```text
xana storage verify
xana storage lock
xana storage unlock
xana storage unlock --recovery-key /absolute/separate/location/xana-recovery.txt
xana storage unlock --recovery-key /absolute/separate/location/xana-recovery.txt --remember
```

`--remember` restores local OS custody after verified independent recovery.
Without it, subsequent opens still need custody or the explicit recovery-file
environment setting. A lost OS credential is recoverable; losing both custody
and the recovery key is not.

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
