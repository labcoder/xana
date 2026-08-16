# Installation, updates, verification, and removal

> Audience: People installing, updating, verifying, or removing Xana.

Xana has a four-platform developer-preview delivery path for Apple Silicon
macOS (`aarch64-apple-darwin`), Intel macOS (`x86_64-apple-darwin`), Windows
x64 (`x86_64-pc-windows-msvc`), and x64 glibc Linux
(`x86_64-unknown-linux-gnu`).

The GitHub commands below apply only after the owner publishes an ordinary
preview. A release workflow run, local version tag, or unpublished draft is not
a public release. If the `latest` URL returns 404, use the locked Git Cargo
or checkout path; do not substitute an unrelated asset or weaken TLS.

Preview binaries are unsigned. macOS archives are not Developer-ID-signed or
notarized, and the Windows executable is not Authenticode-signed. SHA-256
detects bytes that differ from the release manifest. GitHub attestations bind
assets to this repository, workflow, and commit. Neither is an operating-system
publisher signature.

ChatGPT subscription use additionally requires a compatible `codex`
executable on `PATH` or an explicit `codex_program`. API-key and Ollama
connections do not require Codex.

## Install a published preview

The installers require no Rust, Node, Python, repository checkout, elevation,
or system-wide directory. They verify the selected release before activation
and then delegate configuration readiness to Xana.

### macOS or x64 glibc Linux

Review the source-controlled installer before executing the published copy:

```bash
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://github.com/labcoder/xana/releases/latest/download/xana-installer.sh \
  | bash
```

The default destination is `~/.local/bin/xana`. Options are passed after
`bash -s --`:

```bash
# Exact release
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://github.com/labcoder/xana/releases/latest/download/xana-installer.sh \
  | bash -s -- --version 0.6.0

# Custom directory, no setup, and no profile edit
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://github.com/labcoder/xana/releases/latest/download/xana-installer.sh \
  | bash -s -- --install-dir "$HOME/bin" --no-setup --no-modify-path
```

`--modify-path` explicitly adds the selected directory to the user shell
profile. With neither PATH flag, an interactive installer asks and defaults to
no; a noninteractive installer never changes a profile. Start a new shell or
source the named profile after accepting a change. Bash is required. Linux
must be x64 glibc; musl, ARM Linux, and other targets use the source fallback.

### Windows x64 PowerShell

Fetch the published script over HTTPS and invoke the resulting script block;
this does not require changing PowerShell execution policy:

```powershell
$source = Invoke-RestMethod `
  -Uri 'https://github.com/labcoder/xana/releases/latest/download/xana-installer.ps1'
& ([scriptblock]::Create($source))
```

The default destination is
`%LOCALAPPDATA%\Programs\Xana\bin\xana.exe`. The same invocation accepts
parameters:

```powershell
$source = Invoke-RestMethod `
  -Uri 'https://github.com/labcoder/xana/releases/latest/download/xana-installer.ps1'

# Exact release
& ([scriptblock]::Create($source)) -Version 0.6.0

# Custom directory, no setup, and no user-PATH change
& ([scriptblock]::Create($source)) `
  -InstallDir "$env:LOCALAPPDATA\Programs\Xana\bin" `
  -NoSetup `
  -NoModifyPath
```

`-ModifyPath` explicitly changes the user PATH. With neither PATH switch, an
interactive invocation asks and defaults to no; redirected/noninteractive use
does not mutate PATH. Open a new terminal after accepting. Windows ARM64, x86,
and emulated x64 processes are not preview targets.

`XANA_HOME` never chooses the executable destination on either platform.

## Understand the install receipt

The wrapper reports binary state separately from setup state:

- `install`, `reinstall`, `upgrade`, or `downgrade` describes executable
  activation relative to the previous recognizable Xana version;
- `setup=ready-or-configured` means existing valid setup was preserved or an
  interactive setup transaction completed;
- `setup=pending` means verified binary activation succeeded but a person must
  run `xana setup`; and
- `setup=skipped` means `--no-setup`/`-NoSetup` was selected.

The wrappers never parse configuration, inspect credentials, log in, select a
provider, or grant permissions. `xana setup --if-needed` owns readiness.
Healthy state returns without provider, credential, or filesystem effects.
Interactive missing/invalid/incompatible state enters the canonical confirmed
flow; noninteractive state returns a versioned `XANA_SETUP_RESULT` and exit 10.
Unexpected readiness errors remain installer errors after an otherwise
successful binary activation.

Finish or inspect setup with:

```bash
xana setup
xana doctor
xana config check
xana
```

## Verify an archive manually

Choose exactly one archive from the GitHub release matching the target list at
the top of this page. Download that archive, its `.sha256` sidecar,
`xana-release-manifest.txt`, and `sha256.sum` from the same immutable version.
Do not mix `latest` redirects with a pinned asset after selection.

macOS or Linux:

```bash
# Replace TARGET and VERSION with one supported exact pair.
VERSION=0.6.0
TARGET=x86_64-unknown-linux-gnu
ASSET="xana-${TARGET}.tar.gz"
ROOT="xana-${TARGET}"
BASE="https://github.com/labcoder/xana/releases/download/v${VERSION}"

curl --proto '=https' --tlsv1.2 --fail --location --remote-name \
  "${BASE}/${ASSET}"
curl --proto '=https' --tlsv1.2 --fail --location --remote-name \
  "${BASE}/${ASSET}.sha256"

# Linux
sha256sum --check "${ASSET}.sha256"
# macOS
shasum -a 256 --check "${ASSET}.sha256"

gh attestation verify "${ASSET}" --repo labcoder/xana
tar -tzf "${ASSET}"
tar -xzf "${ASSET}"
"./${ROOT}/xana" --version
"./${ROOT}/xana" --help
```

Windows PowerShell:

```powershell
$version = '0.6.0'
$asset = 'xana-x86_64-pc-windows-msvc.zip'
$base = "https://github.com/labcoder/xana/releases/download/v$version"
Invoke-WebRequest -Uri "$base/$asset" -OutFile $asset
Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile "$asset.sha256"

$expected = ((Get-Content -Raw "$asset.sha256").Trim() -split '\s+')[0]
$actual = (Get-FileHash -Algorithm SHA256 $asset).Hash
if ($actual -ine $expected) { throw 'Xana archive SHA-256 mismatch' }

gh attestation verify $asset --repo labcoder/xana
Expand-Archive -LiteralPath $asset -DestinationPath .\xana-preview
.\xana-preview\xana.exe --version
.\xana-preview\xana.exe --help
```

The GitHub CLI is needed only for the optional provenance command. A failed
checksum or attestation is a stop condition: delete the download and inspect
the release/tag/workflow rather than executing it.

## Install from Git or a checkout

The source channel requires Git and Rust from [rustup](https://rustup.rs/).
Xana pins Rust `1.97.1` and uses its checked-in lockfile. It is not published to
crates.io.

Install the default branch or one exact published tag:

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
cargo install --git https://github.com/labcoder/xana.git --tag v0.6.0 --locked
```

For development from a checkout:

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo install --path . --locked
xana --version
```

Use `--rev COMMIT_SHA` instead of `--tag` for an exact unreleased revision.
When running without installing, pass Xana arguments after Cargo's separator:

```bash
cargo run -- setup
cargo run -- connection status codex
cargo run -- model list --connection codex
cargo run -- --plain
```

`cargo init` creates a Rust package; it does not initialize Xana.

## Choose Xana's home

An unset `XANA_HOME` uses platform-standard data/configuration locations. When
set, it must be a nonempty native absolute path. Xana does not expand `~`.

macOS or Linux:

```bash
export XANA_HOME="$HOME/.xana"
```

Windows PowerShell:

```powershell
$env:XANA_HOME = 'C:\Users\you\.xana'
```

Windows Git Bash must convert its shell path for the Windows executable:

```bash
export XANA_HOME="$(cygpath -m "$HOME/.xana")"
```

Do not pass Git Bash `/c/...` or a literal `~/.xana` to the Windows binary.

## Update or select an older preview

Rerun the same installer. Omit the version for the newest published ordinary
preview, or pass an exact version for a repeatable reinstall, upgrade, or
downgrade. The selected manifest and archive are bound to one version before
activation.

There is no `xana update`, background or launch-time update, channel selector,
retained installer rollback, Homebrew/WinGet/Linux package, or automatic
desktop update. Source installations update by repeating the locked Cargo
command at a reviewed revision.

## Remove only the installed executable

Removal is manual and deliberately narrow. It is not `xana reset` and never
deletes configuration, sessions, artifacts, credentials, workspaces, or
external Codex authentication/conversations.

macOS/Linux default:

```bash
rm -- "$HOME/.local/bin/xana"
```

If the installer added PATH, open the profile it reported (`~/.zprofile`,
`~/.bash_profile`, or `~/.profile`) and remove only the line ending:

```text
# xana-installer-path-v1
```

For a custom directory, remove only its `xana` file and the exact marked line
the installer added. Do not recursively remove the custom directory.

Windows default:

```powershell
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\Xana\bin'
Remove-Item -LiteralPath (Join-Path $installDir 'xana.exe')

# Run only if the installer added this exact user-PATH entry.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object {
  -not [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\').Equals(
    $installDir.TrimEnd('\'),
    [StringComparison]::OrdinalIgnoreCase
  )
})
[Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
```

For a Cargo installation, use `cargo uninstall xana`. All removal methods
preserve Xana-owned and vendor-owned personal state.

## Troubleshooting

- **Unsupported target:** use only the exact four targets listed above. There
  is no closest-match fallback. Use the locked Cargo path for other supported
  Rust hosts or wait for Product Distribution work.
- **TLS, redirect, or download failure:** confirm HTTPS access to GitHub and
  retry. Do not disable certificate verification or use an untrusted mirror.
- **Checksum or provenance failure:** do not extract or run the archive. Confirm
  the version/tag and release workflow; report a mismatch with no secret logs.
- **Command not found after installation:** open a new terminal. If PATH was not
  accepted, add the reported directory manually or invoke the absolute binary.
- **Locked Windows destination:** close running Xana processes and retry. The
  installer preserves the prior executable when replacement cannot occur.
- **Setup pending:** binary installation succeeded. Run `xana setup`, then
  `xana doctor`. Invalid/incompatible repair remains explicit, confirmed,
  backed up, and atomic.
- **Unsigned OS warning:** verify SHA-256 and GitHub provenance, then follow
  your operating-system or organization trust policy. Xana does not recommend
  disabling Gatekeeper, SmartScreen, antivirus, or PowerShell execution policy.
- **Wrong `XANA_HOME`:** use a native absolute path as shown above. Executable
  placement and state placement are independent.

Report reproducible installation failures at the repository
[issue tracker](https://github.com/labcoder/xana/issues) without credentials,
raw invalid config, authentication headers, or private owner paths.
