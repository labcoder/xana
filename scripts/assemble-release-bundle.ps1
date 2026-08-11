[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ArtifactDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')][string]$Version,
    [Parameter(Mandatory = $true)][ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')][string]$Tag
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$root = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$archives = @(
    "xana-aarch64-apple-darwin.tar.gz",
    "xana-x86_64-apple-darwin.tar.gz",
    "xana-x86_64-pc-windows-msvc.zip",
    "xana-x86_64-unknown-linux-gnu.tar.gz"
)
$initial = @($archives | ForEach-Object { $_; "$_.sha256" }) + @("dist-manifest.json")
$actualInitial = @(Get-ChildItem -File -LiteralPath $root | ForEach-Object Name | Sort-Object)
$difference = @(Compare-Object ($initial | Sort-Object) $actualInitial)
if ($difference.Count -ne 0) {
    throw "assembled inputs differ from the exact native artifact inventory"
}

foreach ($archive in $archives) {
    $archivePath = Join-Path $root $archive
    $sidecarPath = "$archivePath.sha256"
    $sidecar = (Get-Content -Raw -LiteralPath $sidecarPath).Trim() -split '\s+'
    $sidecarName = if ($sidecar.Count -ge 2) { $sidecar[1].TrimStart('*') } else { "" }
    if ($sidecar.Count -ne 2 -or $sidecar[0] -cnotmatch '^[0-9a-fA-F]{64}$' -or
        $sidecarName -cne $archive) {
        throw "invalid checksum sidecar: $archive.sha256"
    }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash
    if ($actualHash -cne $sidecar[0].ToUpperInvariant()) {
        throw "checksum sidecar does not match $archive"
    }
}

$distManifestPath = Join-Path $root "dist-manifest.json"
$distManifest = Get-Content -Raw -LiteralPath $distManifestPath | ConvertFrom-Json
if ($distManifest.dist_version -ne "0.32.0" -or
    $distManifest.announcement_tag -ne $Tag -or
    @($distManifest.releases).Count -ne 1 -or
    $distManifest.releases[0].app_name -ne "xana" -or
    $distManifest.releases[0].app_version -ne $Version) {
    throw "dist manifest does not match the pinned Xana release"
}

& "$PSScriptRoot/new-release-manifest.ps1" `
    -ArtifactDirectory $root `
    -Version $Version `
    -OutputPath (Join-Path $root "xana-release-manifest.txt") *> $null

$repository = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$releaseNotesSource = Join-Path $repository "docs\releases\$Version.md"
if (-not (Test-Path -LiteralPath $releaseNotesSource -PathType Leaf)) {
    throw "missing checked-in release notes for $Version"
}
$notes = Get-Content -Raw -LiteralPath $releaseNotesSource
if ($notes -notmatch '(?i)unsigned' -or $notes -notmatch '(?i)notariz' -or $notes -notmatch '(?i)authenticode') {
    throw "release notes must state the unsigned preview limitations"
}

$copies = @{
    (Join-Path $repository "install\install.sh") = "xana-installer.sh"
    (Join-Path $repository "install\install.ps1") = "xana-installer.ps1"
    $releaseNotesSource = "xana-$Version-release-notes.md"
    (Join-Path $repository "docs\development\release-review-checklist.md") = "xana-release-review-checklist.md"
}
foreach ($source in $copies.Keys) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "missing release source asset: $source"
    }
    [IO.File]::Copy($source, (Join-Path $root $copies[$source]), $false)
}

$checksumNames = @(
    Get-ChildItem -File -LiteralPath $root |
        Where-Object Name -ne "sha256.sum" |
        ForEach-Object Name |
        Sort-Object
)
$checksumLines = @($checksumNames | ForEach-Object {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $root $_)).Hash.ToLowerInvariant()
    "$hash  $_"
})
[IO.File]::WriteAllLines(
    (Join-Path $root "sha256.sum"),
    $checksumLines,
    [Text.UTF8Encoding]::new($false)
)

$expectedFinal = @($initial) + @(
    "xana-release-manifest.txt",
    "xana-installer.sh",
    "xana-installer.ps1",
    "xana-$Version-release-notes.md",
    "xana-release-review-checklist.md",
    "sha256.sum"
)
$actualFinal = @(Get-ChildItem -File -LiteralPath $root | ForEach-Object Name | Sort-Object)
$difference = @(Compare-Object ($expectedFinal | Sort-Object) $actualFinal)
if ($difference.Count -ne 0) {
    throw "final release bundle differs from the exact fifteen-asset inventory"
}
Write-Output "release bundle verified: Xana $Version, fifteen assets, exact four-target checksums"
