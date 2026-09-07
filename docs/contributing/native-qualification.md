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

Each check records the commit, dirty-tree flag, lockfile hash, toolchain,
OS/architecture, CPU count, runtime-available memory, runner image, start/end,
status, and test totals where applicable. Command output is streamed to logs.
Empty selections and compile-only logs cannot pass a test gate; custody must
execute its exact test once. Failed checks do not suppress independent later
checks, and upload runs even after failure. A cancelled/timed-out/missing gate is
incomplete evidence, not Pass. Jobs have a two-hour limit; custody has 15 minutes.

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

## What a green result does not establish

- Unix browser support: production browser process ownership is currently
  Windows-only. Linux/macOS need an implemented, qualified containment/lifecycle
  path before browser parity can be accepted.
- Interactive key-store denial/locked-service prompts, login/startup or actual
  sleep/wake, dedicated-browser manual sign-in, and genuine account integrations.
- Real TUI/Desktop selection, clipboard, scroll, keyboard/IME/accessibility,
  visual quality or FPS. Headless tests cannot establish those results.
- Release-profile resource measurements and the full native archive/release
  workflow. Existing ignored resource probes are not silently counted as runs.
  These are separate acceptance checks; ordinary source CI is not release proof.

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
