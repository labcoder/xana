[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')]
    [string]$Version,
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$artifacts = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $artifacts "xana-release-manifest.txt"
}

$targets = @(
    @{ Target = "aarch64-apple-darwin"; Extension = ".tar.gz" },
    @{ Target = "x86_64-apple-darwin"; Extension = ".tar.gz" },
    @{ Target = "x86_64-pc-windows-msvc"; Extension = ".zip" },
    @{ Target = "x86_64-unknown-linux-gnu"; Extension = ".tar.gz" }
)

$lines = [Collections.Generic.List[string]]::new()
$lines.Add("manifest-version 1")
$lines.Add("release-version $Version")
foreach ($entry in $targets) {
    $name = "xana-cli-$($entry.Target)$($entry.Extension)"
    $path = Join-Path $artifacts $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "missing release archive: $name"
    }
    $item = Get-Item -LiteralPath $path
    if ($item.Length -le 0 -or $item.Length -gt 134217728) {
        throw "release archive has an invalid size: $name"
    }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    $lines.Add("artifact $($entry.Target) $name $hash $($item.Length)")
}

$parent = Split-Path -Parent $OutputPath
if (-not [string]::IsNullOrWhiteSpace($parent)) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
}
[IO.File]::WriteAllLines($OutputPath, $lines, [Text.UTF8Encoding]::new($false))
Write-Output "release manifest created: $OutputPath"
