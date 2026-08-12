$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$readme = Get-Content -Raw -LiteralPath "README.md"
$installation = Get-Content -Raw -LiteralPath "docs/user/installation.md"
$development = Get-Content -Raw -LiteralPath "docs/development/release-preview.md"
$workspaceManifest = Get-Content -Raw -LiteralPath "Cargo.toml"
$versionMatch = [regex]::Match(
    $workspaceManifest,
    '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(?<version>[0-9]+\.[0-9]+\.[0-9]+)"'
)
if (-not $versionMatch.Success) { throw "could not read workspace version" }
$version = $versionMatch.Groups["version"].Value
$releaseNotes = Get-Content -Raw -LiteralPath "docs/releases/$version.md"
$combined = $readme + $installation + $development + $releaseNotes

$required = @(
    "xana-installer.sh",
    "xana-installer.ps1",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "xana-release-manifest.txt",
    "gh attestation verify",
    "--version $version",
    "-Version $version",
    "--no-setup",
    "-NoSetup",
    "--modify-path",
    "-ModifyPath",
    "xana-installer-path-v1",
    "cargo install --git https://github.com/labcoder/xana.git --locked",
    "cargo uninstall xana",
    "not Authenticode-signed",
    "notarized",
    "not a public release"
)
foreach ($text in $required) {
    if (-not $combined.Contains($text)) {
        throw "installation documentation is missing required contract: $text"
    }
}

$stale = @(
    "# Source installation",
    "has no prebuilt installer",
    "distributed as Rust source"
)
foreach ($text in $stale) {
    if ($readme.Contains($text) -or $installation.Contains($text)) {
        throw "installation documentation retains stale source-only claim: $text"
    }
}

$unsafe = @(
    '(?m)curl[^\r\n]*(--insecure|-k(?:\s|$))',
    '(?i)Set-ExecutionPolicy',
    '(?i)ExecutionPolicy\s+Bypass',
    '(?i)spctl\s+--master-disable',
    '(?i)xattr\s+-d',
    '(?i)Unblock-File'
)
foreach ($pattern in $unsafe) {
    if ($installation -match $pattern -or $readme -match $pattern) {
        throw "user installation documentation contains an unsafe bypass recipe: $pattern"
    }
}

$links = @([regex]::Matches($installation + $readme, '\[[^\]]+\]\((?<path>[^)#]+)(?:#[^)]+)?\)'))
foreach ($link in $links) {
    $path = $link.Groups["path"].Value
    if ($path -match '^(https?://|#)') { continue }
    $base = if ($link.Index -lt $installation.Length) { "docs/user" } else { "." }
    $candidate = Join-Path $base $path
    if (-not (Test-Path -LiteralPath $candidate)) {
        throw "installation documentation has a missing local link: $path"
    }
}

Write-Output "installation docs verified: channels, targets, trust, setup, updates, removal, links, no bypass recipes"
