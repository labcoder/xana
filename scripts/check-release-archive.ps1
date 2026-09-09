[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Archive,
    [Parameter(Mandatory = $true)]
    [ValidateSet(
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu"
    )]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string]$ExpectedVersion,
    [string]$SummaryOutput,
    [string]$MetricsOutput
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. "$PSScriptRoot/release-archive-contract.ps1"
. "$PSScriptRoot/fixture-cleanup.ps1"

$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$archiveBytes = (Get-Item -LiteralPath $archivePath).Length
$checksumPath = "$archivePath.sha256"
if (-not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
    throw "missing checksum sidecar for $archivePath"
}
$expectedHash = ((Get-Content -Raw -LiteralPath $checksumPath).Trim() -split '\s+')[0].ToLowerInvariant()
$actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) {
    throw "archive SHA-256 mismatch"
}

$entries = if ($archivePath.EndsWith(".zip", [StringComparison]::OrdinalIgnoreCase)) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        @($zip.Entries | Where-Object { -not [string]::IsNullOrEmpty($_.Name) } | ForEach-Object { $_.FullName })
    } finally {
        $zip.Dispose()
    }
} else {
    $listed = @(& tar -tzf $archivePath)
    if ($LASTEXITCODE -ne 0) {
        throw "could not list release archive"
    }
    @($listed | Where-Object { -not $_.EndsWith("/") })
}

$executable = if ($Target -eq "x86_64-pc-windows-msvc") { "xana.exe" } else { "xana" }
$layout = Resolve-ReleaseArchiveLayout -Entries $entries -Target $Target

$stagingParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$staging = Join-Path $stagingParent ("xana-archive-audit-" + [Guid]::NewGuid().ToString("N"))
$binaryBytes = 0L
New-Item -ItemType Directory -Path $staging | Out-Null
try {
    if ($archivePath.EndsWith(".zip", [StringComparison]::OrdinalIgnoreCase)) {
        [System.IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $staging)
    } else {
        & tar -xzf $archivePath -C $staging
        if ($LASTEXITCODE -ne 0) {
            throw "could not extract release archive"
        }
    }

    $payloadDirectory = if ([string]::IsNullOrEmpty($layout.PayloadRoot)) {
        $staging
    } else {
        Join-Path $staging $layout.PayloadRoot
    }
    $binary = Join-Path $payloadDirectory $executable
    $binaryBytes = (Get-Item -LiteralPath $binary).Length
    $versionOutput = (& $binary --version 2>&1 | Out-String).Trim()
    Assert-ReleaseArchiveVersion -Output $versionOutput -ExpectedVersion $ExpectedVersion -ExitCode $LASTEXITCODE
    & $binary --help *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "staged Xana help smoke failed"
    }
} finally {
    $fullStaging = [IO.Path]::GetFullPath($staging)
    if (-not (Test-SafeFixtureCleanupRoot `
            -Parent $stagingParent -Root $fullStaging `
            -Separator ([IO.Path]::DirectorySeparatorChar) `
            -RequiredPrefix "xana-archive-audit-")) {
        throw "refusing unsafe release-archive audit cleanup"
    }
    if (Test-Path -LiteralPath $fullStaging) {
        [IO.Directory]::Delete($fullStaging, $true)
    }
}

if (-not [string]::IsNullOrWhiteSpace($SummaryOutput)) {
    $archiveMiB = [Math]::Round($archiveBytes / 1MB, 2)
    $binaryMiB = [Math]::Round($binaryBytes / 1MB, 2)
    $summary = @"
### Xana release size

| Target | Executable | Archive |
| --- | ---: | ---: |
| ``$Target`` | $binaryMiB MiB ($binaryBytes bytes) | $archiveMiB MiB ($archiveBytes bytes) |

This is an observation only; no size budget is enforced.
"@
    [IO.File]::AppendAllText($SummaryOutput, $summary, [Text.UTF8Encoding]::new($false))
}

if (-not [string]::IsNullOrWhiteSpace($MetricsOutput)) {
    $metricsParent = Split-Path -Parent $MetricsOutput
    if (-not [string]::IsNullOrWhiteSpace($metricsParent)) {
        [IO.Directory]::CreateDirectory($metricsParent) | Out-Null
    }
    $metrics = [ordered]@{
        schema_version = 1
        target = $Target
        xana_version = ($versionOutput -split '\s+', 2)[1]
        executable_bytes = $binaryBytes
        archive_bytes = $archiveBytes
    } | ConvertTo-Json
    [IO.File]::WriteAllText(
        $MetricsOutput,
        "$metrics`n",
        [Text.UTF8Encoding]::new($false)
    )
}

Write-Output "release archive metrics: target=$Target executable_bytes=$binaryBytes archive_bytes=$archiveBytes"
Write-Output "release archive verified: $Target, SHA-256, bounded contents, version/help smoke"
