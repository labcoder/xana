#!/usr/bin/env pwsh

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = Split-Path -Parent $PSScriptRoot
Push-Location $repository
try {
    $cliTree = (& cargo tree --locked -p xana -e normal --prefix none 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "could not inspect the Xana CLI/TUI dependency graph`n$cliTree"
    }
    if ($cliTree -match '(?m)^gpui(?:[-_ ]|$)') {
        throw 'the root xana package unexpectedly resolves a GPUI dependency'
    }

    $metadataText = (& cargo metadata --locked --format-version 1 --no-deps 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "could not inspect direct Desktop dependencies`n$metadataText"
    }
    $metadata = $metadataText | ConvertFrom-Json
    $desktopPackage = @($metadata.packages | Where-Object { $_.name -eq 'xana-desktop' })
    if ($desktopPackage.Count -ne 1) {
        throw "expected one xana-desktop package, found $($desktopPackage.Count)"
    }
    $allowedDesktopDependencies = @(
        'gpui',
        'gpui-ai',
        'gpui-component',
        'gpui-component-assets',
        'gpui_platform',
        'markdown',
        'url',
        'xana'
    )
    $unexpectedDesktopDependencies = @(
        $desktopPackage[0].dependencies |
            Where-Object { $null -eq $_.kind -and $_.name -notin $allowedDesktopDependencies } |
            ForEach-Object { $_.name }
    )
    if ($unexpectedDesktopDependencies.Count -ne 0) {
        throw "Desktop gained unreviewed direct dependencies: $($unexpectedDesktopDependencies -join ', ')"
    }

    $desktopSource = @(
        Get-ChildItem (Join-Path $repository 'crates/xana-desktop/src') -Recurse -File -Filter '*.rs' |
            ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }
    ) -join "`n"
    $forbiddenAuthority = [ordered]@{
        'process spawning' = '(?m)\b(?:std|tokio)::process\b|\bprocess::Command\b'
        'raw networking' = '(?m)\b(?:std|tokio)::net\b|\b(?:TcpStream|UdpSocket|TcpListener)\b'
        'provider HTTP' = '(?m)\b(?:reqwest|hyper)::'
        'credential-store access' = '(?m)\bkeyring::'
    }
    foreach ($entry in $forbiddenAuthority.GetEnumerator()) {
        if ($desktopSource -match $entry.Value) {
            throw "Desktop presentation source gained $($entry.Key) authority; keep it behind xana::desktop"
        }
    }

    $gpuiTree = (& cargo tree --locked -p xana-desktop -i gpui --prefix none 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "could not resolve one unambiguous GPUI source family`n$gpuiTree"
    }
    $gpuiRoots = @(
        $gpuiTree -split "`r?`n" |
            Where-Object { $_ -match '^gpui v' }
    )
    if ($gpuiRoots.Count -ne 1) {
        throw "expected exactly one GPUI root, found $($gpuiRoots.Count)"
    }
    if ($gpuiRoots[0] -notmatch 'github\.com/zed-industries/zed#f66ed399') {
        throw "Desktop resolved an unexpected GPUI revision: $($gpuiRoots[0])"
    }

    $manifest = Get-Content -Raw (Join-Path $repository 'Cargo.toml')
    foreach ($pin in @(
        '40ec5cf4e8b28f94b8fbbb4cb2f90d177c946039',
        '5cb094628d27acbd557a1c22fd830417a702f0e5'
    )) {
        if (-not $manifest.Contains($pin)) {
            throw "workspace manifest is missing reviewed Desktop dependency pin $pin"
        }
    }

    Write-Host 'desktop dependency boundary verified: CLI/TUI isolated; direct dependencies and privileged authorities bounded; one reviewed GPUI source family'
}
finally {
    Pop-Location
}
