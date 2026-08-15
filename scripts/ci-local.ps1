[CmdletBinding()]
param(
    [ValidateRange(1, 64)]
    [int]$TestThreads = 4,
    [string]$TargetDirectory = "target/ci-local",
    [switch]$RequireClean,
    [switch]$IncludeReleasePlan
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-CiStep {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Action
    )

    Write-Output "`n==> $Name"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$resolvedTarget = if ([IO.Path]::IsPathRooted($TargetDirectory)) {
    [IO.Path]::GetFullPath($TargetDirectory)
} else {
    [IO.Path]::GetFullPath((Join-Path $repositoryRoot $TargetDirectory))
}
$previousTarget = $env:CARGO_TARGET_DIR
$previousIncremental = $env:CARGO_INCREMENTAL
$bash = if ($IsWindows -and (Test-Path -LiteralPath "C:\Program Files\Git\bin\bash.exe")) {
    "C:\Program Files\Git\bin\bash.exe"
} else {
    (Get-Command bash -ErrorAction Stop).Source
}

Push-Location $repositoryRoot
try {
    $env:CARGO_TARGET_DIR = $resolvedTarget
    $env:CARGO_INCREMENTAL = "0"

    $rustVersion = (& rustc --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $rustVersion -cnotmatch '^rustc 1\.97\.1(?:\s|$)') {
        throw "CI requires Rust 1.97.1; active compiler is '$rustVersion'"
    }

    ./scripts/report-ci-environment.ps1
    Write-Output "CI Bash: $bash"
    Invoke-CiStep "Formatting" { & cargo fmt --all --check }
    Invoke-CiStep "Clippy" {
        & cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    }
    Invoke-CiStep "All-feature tests ($TestThreads threads)" {
        & cargo test --locked --workspace --all-targets --all-features -- "--test-threads=$TestThreads"
    }
    Invoke-CiStep "No-default-feature tests ($TestThreads threads)" {
        & cargo test --locked --workspace --all-targets --no-default-features -- "--test-threads=$TestThreads"
    }
    Invoke-CiStep "MCP stdio lifecycle stress" {
        & cargo test --locked --lib --all-features `
            mcp::stdio::tests::stdio_lifecycle_is_stable_under_repetition -- `
            --ignored --exact
    }
    Invoke-CiStep "Package contents" {
        if ($RequireClean) {
            ./scripts/check-package-contents.ps1
        } else {
            ./scripts/check-package-contents.ps1 -AllowDirty
        }
    }
    Invoke-CiStep "Bash installer behavior" {
        & $bash ./scripts/test-install-sh.sh
        if ($LASTEXITCODE -eq 0) { & $bash ./scripts/test-release-sha256.sh }
    }
    if ($IsWindows) {
        Invoke-CiStep "PowerShell installer behavior" { ./scripts/test-install-ps1.ps1 }
    }
    Invoke-CiStep "Release workflow contracts" {
        ./scripts/check-release-workflow.ps1
        ./scripts/test-release-ci-evidence.ps1
        ./scripts/test-create-release-draft.ps1
        ./scripts/test-release-archive-contract.ps1
        ./scripts/test-release-bundle.ps1
    }
    Invoke-CiStep "Installation documentation and removal" {
        ./scripts/check-installation-docs.ps1
        ./scripts/test-removal-recipes.ps1
    }

    $packageArguments = @("package", "--package", "xana", "--locked")
    if (-not $RequireClean) { $packageArguments += "--allow-dirty" }
    Invoke-CiStep "Packageable application" {
        $env:CARGO_TARGET_DIR = "$resolvedTarget-package"
        try {
            & cargo @packageArguments
        } finally {
            $env:CARGO_TARGET_DIR = $resolvedTarget
        }
    }

    if ($IncludeReleasePlan) {
        $distVersion = (& dist --version | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $distVersion -cnotmatch '0\.32\.0(?:\s|$)') {
            throw "release-plan validation requires preinstalled cargo-dist 0.32.0"
        }
        Invoke-CiStep "Release Preview plan" { ./scripts/check-release-plan.ps1 }
    }
} finally {
    if ($null -eq $previousTarget) {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    } else {
        $env:CARGO_TARGET_DIR = $previousTarget
    }
    if ($null -eq $previousIncremental) {
        Remove-Item Env:CARGO_INCREMENTAL -ErrorAction SilentlyContinue
    } else {
        $env:CARGO_INCREMENTAL = $previousIncremental
    }
    Pop-Location
}
