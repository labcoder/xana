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
    [string]$Target
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$archivePath = (Resolve-Path -LiteralPath $Archive).Path
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

foreach ($entry in $entries) {
    $normalized = $entry.Replace("\", "/")
    if ($normalized.StartsWith("/") -or $normalized -match '(^|/)\.\.(/|$)' -or $normalized.Contains(":")) {
        throw "unsafe archive entry: $entry"
    }
}

$executable = if ($Target -eq "x86_64-pc-windows-msvc") { "xana.exe" } else { "xana" }
$expectedEntries = @("LICENSE", "README.md", "installation.md", $executable) | Sort-Object
$actualEntries = @($entries | ForEach-Object { $_.Replace("\", "/") } | Sort-Object)
$difference = @(Compare-Object $actualEntries $expectedEntries)
if ($difference.Count -ne 0) {
    throw "archive inventory differs from the bounded release contract: $($actualEntries -join ', ')"
}

$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("xana-archive-audit-" + [Guid]::NewGuid().ToString("N"))
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

    $binary = Join-Path $staging $executable
    $versionOutput = (& $binary --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch '^xana 0\.\d+\.\d+') {
        throw "staged Xana version smoke failed: $versionOutput"
    }
    & $binary --help *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "staged Xana help smoke failed"
    }
} finally {
    $resolvedStaging = (Resolve-Path -LiteralPath $staging).Path
    $tempRoot = [System.IO.Path]::GetTempPath().TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if (-not $resolvedStaging.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to clean a non-temporary audit directory"
    }
    Remove-Item -Recurse -Force -LiteralPath $resolvedStaging
}

Write-Output "release archive verified: $Target, SHA-256, bounded contents, version/help smoke"
