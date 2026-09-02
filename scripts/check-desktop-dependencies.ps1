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

    Write-Host 'desktop dependency boundary verified: CLI/TUI isolated; one reviewed GPUI source family'
}
finally {
    Pop-Location
}
