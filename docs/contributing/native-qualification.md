# Native Platform Qualification

> Audience: Contributors and coding agents
> Authority: Descriptive workflow and evidence contract

**Native Platform Qualification** (`native-qualification.yml`) is an opt-in GitHub
Actions workflow. It has only `workflow_dispatch`: it does not run on pushes,
pull requests, tags or a schedule. Ordinary **CI** still runs on main/PR changes;
it checks this workflow's inexpensive script contracts, not its native workload.

Keep this workflow as a reusable qualification lane, not a milestone-specific
check-in. Run it when native dependencies, protected storage, platform lifecycle
behavior or release readiness warrant fresh native evidence. It need not run
for every change; select the reviewed revision and preserve its exact results.

## Run it

1. Push the reviewed commits to `main`. A new manual workflow must exist on the
   default branch before GitHub exposes its Run workflow control.
2. Open **Actions → Native Platform Qualification → Run workflow**, select **main**,
   and run once. All three native jobs run independently:

   | Runner | Native Rust host |
   |---|---|
   | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` |
   | `macos-15` | `aarch64-apple-darwin` |
   | `macos-15-intel` | `x86_64-apple-darwin` |

   Or, from an authenticated GitHub CLI:

   ```text
   gh workflow run native-qualification.yml --repo labcoder/xana --ref main
   gh run list --repo labcoder/xana --workflow native-qualification.yml --limit 5
   gh run view RUN_ID --repo labcoder/xana
   gh run download RUN_ID --repo labcoder/xana --dir native-evidence
   ```

3. Confirm ordinary CI and all three qualification jobs pass on the **same
   commit**. Preserve each target's artifacts and run/attempt URLs before their
   seven-day retention expires. Re-running failed jobs retains the run's source
   revision; dispatch again after a source fix rather than treating an old green
   run as evidence for a new commit.

Dispatch explicitly authorizes fixture credentials and build tools **on these
disposable hosted VMs**. No account secrets or provider keys are needed. There
is no tag, publishing job, Release Preview prerequisite, model download or paid
provider call. GitHub's standard hosted-runner/billing policies still apply.

## What runs

- Pinned Rust, locked dependency sources and the vendored SQLCipher patch, with
  the existing trusted CI dependency cache restored read-only. Cargo builds are
  serialized within each runner, without clearing any caches.
- Formatting, strict all-target/all-feature workspace Clippy, and full workspace
  tests in both feature modes, **including Linux unit-test execution**. A third
  root-only no-default run avoids feature unification through Desktop.
- Existing encrypted storage, backup/restore, crash, cancellation, adapter
  outcomes and governed-vision contracts, using synthetic homes and services;
  the normal opt-in/manual/stress tests remain ignored and counted as such.
- One explicitly selected production OS-custody fixture: reopen, Xana's explicit
  lock/unlock, deleted-credential refusal, independent recovery, and exact cleanup.
- Package inventory/negative fixtures and a locked source install into a fresh
  temporary directory, followed by the installed CLI's `--version`. This is a
  native debug source installation, **not** a release archive or installer audit.
  Desktop is compiled and tested headlessly by the workspace gates, not launched
  in a graphical login session.
- An explicit release-profile resource gate: one optimized library-test build,
  then separate 10k/100k protected-history processes. Each records five independent
  database opens and resume windows, verification, backup and restore. The gate
  also builds CLI/Desktop release executables in separate Cargo invocations and
  records each exact command, size and hash. A combined workspace build can
  unify Desktop-only features into the CLI and is not a standalone CLI baseline.

Each check records the commit, dirty-tree flag, lockfile hash, toolchain,
OS/architecture, CPU count, runtime-available memory, runner image, start/end,
status, and test totals where applicable. Command output is streamed to logs.
Empty selections and compile-only logs cannot pass a test gate; custody must
execute its exact test once. Failed checks do not suppress independent later
checks, and upload runs even after failure. A cancelled/timed-out/missing gate is
incomplete evidence, not Pass. Jobs have a three-hour limit; custody has 15 minutes
and the resource gate has 90 minutes. Each history process has a 30-minute limit.
Ordinary CI allows up to 90 minutes per quality job: a measured cold Windows
run completed all source/test/install gates in 56 minutes, then exhausted its
former one-hour limit while compressing the dependency cache. CI disables unused
dev and test debug-symbol generation consistently, retaining debug assertions
and every test gate. These workflow-only settings do not change developers'
local profiles or optimized release artifacts; cold-cache execution still needs
its own successful hosted result.
The native Linux lane installs Fontconfig, xkbcommon/X11 and XCB development
libraries for actual Desktop test linking, not just `cargo check`.

Protected-storage regressions include legitimate ancestor aliases (such as
macOS `/var`), while still rejecting a symlink at the database file itself.
The vendored-source checksum contract hashes the pinned upstream LF bytes;
changing an expected hash to match a platform's checkout conversion is not a
valid way to approve source drift.

Only `target/native-qualification/*.log` and `*.json` are uploaded. Never expand
this to entire target/temp/home directories: they can contain encrypted homes,
credentials, databases or recovery material. Fixture output uses synthetic data;
do not adapt these commands to personal state or dump environment variables.

## OS custody isolation

The Bash custody launcher refuses local workstations, self-hosted runners,
non-dispatch invocations and root. These are mistake-prevention checks, not a
security boundary against someone editing the workflow or forging its environment.

On Linux, the workflow installs GNOME Keyring and D-Bus on the ephemeral VM.
The fixture runs as the ordinary runner user inside one private `dbus-run-session`,
with separate XDG directories and an unlocked, **nonempty-password** keyring.
Only the owned foreground daemon is stopped. This exercises a real native Secret
Service, not a fake store, root WSL, or a claim about a desktop login session.

On macOS, the launcher creates and unlocks one random-password keychain, makes it
the disposable runner's user-domain default for the production store, and restores
the prior default/search list before deleting only that keychain. A cleanup
failure fails the gate; secrets and leftover fixtures are never uploaded. Abrupt
runner termination relies on VM disposal, not a fabricated cleanup success.

The underlying [custody fixture](native-storage.md#opt-in-native-custody-check)
owns a single generated credential and no existing Xana home. Local shell tests
stub the OS commands to test isolation, failure propagation and cleanup; those
tests **are not native custody qualification**.

## Resource evidence

`qualify-native-resources.ps1` reuses the existing protected-history fixture.
It builds once with the unchanged release profile and runs the exact ignored
test through `measure-history-resources.ps1`; unrelated ignored tests do not run.
The sampler removes a profile-only override from the child environment. It
requires the exact successful test, five distinct trials and all
generation/verification/backup/restore markers before accepting a measurement.

The report preserves all five trials and median/p95 open/resume durations.
These are independent database opens in a fresh process, **not five OS-cold-cache
runs**. RSS and cumulative CPU are sampled every 100 ms across fixture generation,
verification and backup/restore. The maximum observed current working set is not
a guaranteed peak, and the final CPU sample can precede process exit. Desktop
executable size is not an installer size or a GUI-performance measurement.

Failure, malformed output, timeout and incomplete cleanup remain failed reports.
The sampler terminates and waits for its exact owned process before reporting
cleanup; disposing a process handle alone is insufficient. Abrupt runner loss
still relies on disposable VM cleanup and leaves qualification incomplete.
Only synthetic logs and reports are uploaded, never the fixture database/home.
The small offline sampler test exercises real process exit and timeout with a
synthetic worker; it does not count as a protected-history resource run.

Resource artifacts include `history-10000.json`, `history-100000.json` and
`resource-packages.json`, tied to the source/toolchain envelope in `resources.json`.
Preserve the corresponding logs as well as summaries. A past workflow run without
this gate does not establish these measurements for a newer revision.

## What a green result does not establish

- Unix browser support: production browser process ownership is currently
  Windows-only. Linux/macOS need an implemented, qualified containment/lifecycle
  path before browser parity can be accepted.
- Interactive key-store denial/locked-service prompts, login/startup or actual
  sleep/wake, dedicated-browser manual sign-in, and genuine account integrations.
- Real TUI/Desktop selection, clipboard, scroll, keyboard/IME/accessibility,
  visual quality or FPS. Headless tests cannot establish those results.
- Full native archive/release workflow, browser or loaded-client resource
  measurements, and end-user performance acceptance. The backend resource gate
  does not replace those checks; ordinary source CI is not release proof.

Do not remove a remaining acceptance gate solely because this workflow is green.

## Implementation references

- [GitHub manual workflow dispatch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
  defines default-branch discovery and branch selection.
- [Headless Secret Service setup](https://github.com/jaraco/keyring/blob/main/README.rst#using-keyring-on-headless-linux-systems)
  describes keeping the daemon and client in one D-Bus session.
- [GNOME daemon implementation](https://github.com/GNOME/gnome-keyring/blob/master/daemon/gkd-main.c)
  defines foreground/unlock behavior; the pinned Rust Apple store uses
  `SecKeychain::default_for_domain(User)`, which requires the temporary default
  change rather than only changing Keychain's search list.
