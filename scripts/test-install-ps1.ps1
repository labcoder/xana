[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repository = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$installer = Join-Path $repository "install\install.ps1"
$workspaceManifest = Get-Content -Raw -LiteralPath (Join-Path $repository "Cargo.toml")
$versionMatch = [regex]::Match(
    $workspaceManifest,
    '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(?<version>[0-9]+\.[0-9]+\.[0-9]+)"'
)
if (-not $versionMatch.Success) { throw "could not read workspace version" }
$version = $versionMatch.Groups["version"].Value

$binary = Join-Path $repository "target\debug\xana.exe"
& cargo build --package xana-cli
if ($LASTEXITCODE -ne 0) { throw "could not build the installer fixture binary" }
$binaryVersion = (& $binary --version | Out-String).Trim()
if ($binaryVersion -ne "xana $version") { throw "fixture binary version does not match Cargo" }

$testParent = [IO.Path]::GetTempPath()
$testRoot = Join-Path $testParent ("xana-install-ps1-test-" + [Guid]::NewGuid().ToString("N"))
$fixture = Join-Path $testRoot "fixture"
$payload = Join-Path $testRoot "payload"
$installDirectory = Join-Path $testRoot "install"
$userPathFile = Join-Path $testRoot "user-path.txt"
[IO.Directory]::CreateDirectory($fixture) | Out-Null
[IO.Directory]::CreateDirectory($payload) | Out-Null

function New-FixtureArchive {
    param([switch]$ExtraEntry, [switch]$InvalidExecutable)

    Get-ChildItem -Force -LiteralPath $payload | Remove-Item -Force
    [IO.File]::Copy((Join-Path $repository "LICENSE"), (Join-Path $payload "LICENSE"), $true)
    [IO.File]::Copy((Join-Path $repository "README.md"), (Join-Path $payload "README.md"), $true)
    [IO.File]::Copy(
        (Join-Path $repository "docs\user\installation.md"),
        (Join-Path $payload "installation.md"),
        $true
    )
    if ($InvalidExecutable) {
        [IO.File]::WriteAllText((Join-Path $payload "xana.exe"), "not a Windows executable")
    } else {
        [IO.File]::Copy($binary, (Join-Path $payload "xana.exe"), $true)
    }
    if ($ExtraEntry) {
        [IO.File]::WriteAllText((Join-Path $payload "unexpected"), "unexpected")
    }

    $archive = Join-Path $fixture "xana-cli-x86_64-pc-windows-msvc.zip"
    if (Test-Path -LiteralPath $archive) { [IO.File]::Delete($archive) }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($payload, $archive)
}

function Write-FixtureManifest {
    foreach ($name in @(
        "xana-cli-aarch64-apple-darwin.tar.gz",
        "xana-cli-x86_64-apple-darwin.tar.gz",
        "xana-cli-x86_64-unknown-linux-gnu.tar.gz"
    )) {
        [IO.File]::WriteAllText((Join-Path $fixture $name), "fixture $name")
    }
    & (Join-Path $repository "scripts\new-release-manifest.ps1") `
        -ArtifactDirectory $fixture `
        -Version $version *> $null
}

function Quote-ProcessArgument {
    param([string]$Value)
    if ($Value -notmatch '[\s"]') { return $Value }
    '"' + $Value.Replace('\', '\').Replace('"', '\"') + '"'
}

function Invoke-Installer {
    param([string[]]$Arguments)

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = [Diagnostics.ProcessStartInfo]::new()
    $process.StartInfo.FileName = "powershell.exe"
    $allArguments = @(
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        $installer
    ) + $Arguments
    $process.StartInfo.Arguments = ($allArguments | ForEach-Object { Quote-ProcessArgument $_ }) -join ' '
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.RedirectStandardInput = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    if (-not $process.Start()) { throw "could not start PowerShell installer test" }
    $process.StandardInput.Close()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $stdout; Stderr = $stderr }
}

$baseArguments = @(
    "-Version", $version,
    "-InstallDir", $installDirectory,
    "-AllowTestFixture",
    "-TestFixtureRoot", $fixture,
    "-TestTarget", "x86_64-pc-windows-msvc"
)

try {
    New-FixtureArchive
    Write-FixtureManifest

    $first = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    if ($first.ExitCode -ne 0 -or $first.Stdout -notmatch "Xana install: $([regex]::Escape($version))") {
        throw "clean PowerShell fixture install failed: $($first.Stderr)"
    }
    $finalPath = Join-Path $installDirectory "xana.exe"
    $installedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash

    $second = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    if ($second.ExitCode -ne 0 -or $second.Stdout -notmatch "Xana reinstall:") {
        throw "PowerShell reinstall classification failed: $($second.Stderr)"
    }

    $pathArguments = $baseArguments + @(
        "-NoSetup", "-ModifyPath", "-TestUserPathFile", $userPathFile
    )
    $pathResult = Invoke-Installer $pathArguments
    if ($pathResult.ExitCode -ne 0) { throw "test PATH update failed: $($pathResult.Stderr)" }
    $userPath = [IO.File]::ReadAllText($userPathFile)
    if ($userPath -ne $installDirectory) { throw "test PATH did not preserve the exact install directory" }
    $repeatPath = Invoke-Installer $pathArguments
    if ($repeatPath.ExitCode -ne 0 -or [IO.File]::ReadAllText($userPathFile) -ne $installDirectory) {
        throw "test PATH update was not idempotent"
    }

    $priorXanaHome = $env:XANA_HOME
    $env:XANA_HOME = Join-Path $testRoot "missing-xana-home"
    try {
        $setup = Invoke-Installer ($baseArguments + @("-NoModifyPath"))
    } finally {
        $env:XANA_HOME = $priorXanaHome
    }
    if ($setup.ExitCode -ne 0 -or $setup.Stdout -notmatch 'setup=pending' -or
        $setup.Stdout -notmatch 'XANA_SETUP_RESULT') {
        throw "noninteractive setup-pending translation failed: $($setup.Stderr)"
    }

    [IO.File]::AppendAllText(
        (Join-Path $fixture "xana-cli-x86_64-pc-windows-msvc.zip"),
        "corruption"
    )
    $corrupt = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    if ($corrupt.ExitCode -eq 0 -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash -ne $installedHash) {
        throw "corrupt archive did not preserve the prior executable"
    }

    New-FixtureArchive -ExtraEntry
    Write-FixtureManifest
    $unexpected = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    if ($unexpected.ExitCode -eq 0 -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash -ne $installedHash) {
        throw "unexpected ZIP inventory did not preserve the prior executable"
    }

    New-FixtureArchive -InvalidExecutable
    Write-FixtureManifest
    $badSmoke = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    if ($badSmoke.ExitCode -eq 0 -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash -ne $installedHash) {
        throw "staged smoke failure did not preserve the prior executable"
    }

    New-FixtureArchive
    Write-FixtureManifest
    $lock = [IO.File]::Open($finalPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try {
        $locked = Invoke-Installer ($baseArguments + @("-NoSetup", "-NoModifyPath"))
    } finally {
        $lock.Dispose()
    }
    if ($locked.ExitCode -eq 0 -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash -ne $installedHash) {
        throw "locked destination did not preserve the prior executable"
    }

    $pathFailure = Invoke-Installer ($baseArguments + @(
        "-NoSetup", "-ModifyPath", "-TestUserPathFile", $fixture
    ))
    if ($pathFailure.ExitCode -eq 0 -or
        (Get-FileHash -Algorithm SHA256 -LiteralPath $finalPath).Hash -ne $installedHash) {
        throw "PATH failure did not roll back executable activation"
    }

    $combined = $first.Stdout + $first.Stderr + $second.Stdout + $setup.Stdout + $setup.Stderr
    if ($combined.Contains("setup-secret-sentinel")) { throw "seeded secret appeared in installer output" }
    Write-Output "PowerShell installer verified: install/reinstall, exact version, PATH idempotence, setup pending, checksum, ZIP inventory, smoke, lock, PATH rollback"
} finally {
    $fullRoot = [IO.Path]::GetFullPath($testRoot)
    $fullParent = [IO.Path]::GetFullPath($testParent).TrimEnd('\') + '\'
    if (-not $fullRoot.StartsWith($fullParent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($fullRoot).StartsWith("xana-install-ps1-test-")) {
        throw "refusing unsafe installer-test cleanup"
    }
    if (Test-Path -LiteralPath $fullRoot) {
        [IO.Directory]::Delete($fullRoot, $true)
    }
}
