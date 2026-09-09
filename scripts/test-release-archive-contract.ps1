$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($null -eq (Get-Variable -Name IsWindows -ErrorAction SilentlyContinue)) {
    Set-Variable `
        -Name IsWindows `
        -Value $true `
        -Option ReadOnly, AllScope `
        -Scope Script
}

. "$PSScriptRoot/release-archive-contract.ps1"
. "$PSScriptRoot/fixture-cleanup.ps1"
Assert-FixtureCleanupGuardContract

function Assert-Rejected {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Entries,
        [Parameter(Mandatory = $true)]
        [string]$Target
    )

    $rejected = $false
    try {
        Resolve-ReleaseArchiveLayout -Entries $Entries -Target $Target *> $null
    } catch {
        $rejected = $true
    }
    if (-not $rejected) {
        throw "unsafe or unexpected archive layout was accepted for $Target"
    }
}

$linuxTarget = "x86_64-unknown-linux-gnu"
$linuxRoot = "xana-$linuxTarget"
$linuxEntries = @(
    "$linuxRoot/LICENSE",
    "$linuxRoot/README.md",
    "$linuxRoot/installation.md",
    "$linuxRoot/xana"
)
$linux = Resolve-ReleaseArchiveLayout -Entries $linuxEntries -Target $linuxTarget
if ($linux.PayloadRoot -cne $linuxRoot -or $linux.Entries.Count -ne 4) {
    throw "canonical cargo-dist Unix layout was not resolved"
}

$windowsEntries = @("LICENSE", "README.md", "installation.md", "xana.exe")
$windows = Resolve-ReleaseArchiveLayout `
    -Entries $windowsEntries `
    -Target "x86_64-pc-windows-msvc"
if ($windows.PayloadRoot -cne "" -or $windows.Entries.Count -ne 4) {
    throw "canonical cargo-dist Windows layout was not resolved"
}

Assert-Rejected `
    -Entries @("LICENSE", "README.md", "installation.md", "xana") `
    -Target $linuxTarget
Assert-Rejected `
    -Entries @(
        "wrong-root/LICENSE",
        "wrong-root/README.md",
        "wrong-root/installation.md",
        "wrong-root/xana"
    ) `
    -Target $linuxTarget
Assert-Rejected -Entries ($linuxEntries + "$linuxRoot/unexpected") -Target $linuxTarget
Assert-Rejected -Entries ($linuxEntries + "$linuxRoot/xana") -Target $linuxTarget
Assert-Rejected `
    -Entries @(
        "$linuxRoot/LICENSE",
        "$linuxRoot/README.md",
        "$linuxRoot/installation.md",
        "$linuxRoot/../xana"
    ) `
    -Target $linuxTarget
Assert-Rejected `
    -Entries @(
        "xana-x86_64-pc-windows-msvc/LICENSE",
        "xana-x86_64-pc-windows-msvc/README.md",
        "xana-x86_64-pc-windows-msvc/installation.md",
        "xana-x86_64-pc-windows-msvc/xana.exe"
    ) `
    -Target "x86_64-pc-windows-msvc"

Assert-ReleaseArchiveVersion -Output "xana 0.8.0" -ExpectedVersion "0.8.0" -ExitCode 0
foreach ($case in @(
    @("xana 0.7.0", 0),
    @("xana 0.8.0-extra", 0),
    @("xana 0.8.0`nignored error", 0),
    @("xana 0.8.0", 1)
)) {
    $rejected = $false
    try {
        Assert-ReleaseArchiveVersion -Output $case[0] -ExpectedVersion "0.8.0" -ExitCode $case[1]
    } catch { $rejected = $true }
    if (-not $rejected) { throw "wrong version or failed executable passed the release archive audit" }
}

Write-Output "release archive contract verified: canonical roots, exact inventory/version, unsafe-layout and failed-smoke rejection"
