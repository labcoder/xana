$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/native-qualification.ps1"
& "$PSScriptRoot/test-history-resource-evidence.ps1"
& "$PSScriptRoot/test-history-resource-sampler.ps1"

function Assert-Rejected {
    param([scriptblock]$Action)
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'invalid native qualification evidence was accepted' }
}

$pass = 'test result: ok. 3 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s'
$totals = Get-NativeTestEvidence "$pass`n$pass"
if ($totals.passed -ne 6 -or $totals.ignored -ne 4) { throw 'workspace evidence did not aggregate' }
Assert-Rejected { Get-NativeTestEvidence 'Finished test profile; executable built' }
Assert-Rejected { Get-NativeTestEvidence ($pass -replace '3 passed', '0 passed') }
Assert-Rejected { Get-NativeTestEvidence "$pass`ntest result: FAILED. 0 passed; 1 failed; 0 ignored;" }
$exact = "test $NativeCustodyTest ... ok`ntest result: ok. 1 passed; 0 failed; 0 ignored;"
Get-NativeTestEvidence $exact -Custody | Out-Null
Assert-Rejected { Get-NativeTestEvidence ($exact.Replace($NativeCustodyTest, 'some_other_test')) -Custody }
Assert-Rejected { Get-NativeTestEvidence "$exact`n$exact" -Custody }
Assert-Rejected { Get-NativeTestEvidence ($exact -replace '0 ignored', '1 ignored') -Custody }
Assert-Rejected { Get-NativeQualificationCommand 'unreviewed-shell-command' }
foreach ($check in @('all-features', 'no-default', 'root-no-default')) {
    $command = Get-NativeQualificationCommand $check
    if ($command.Program -ne 'cargo' -or $command.Arguments[0] -ne 'test' -or
        '--locked' -notin $command.Arguments -or '--all-targets' -notin $command.Arguments) {
        throw 'native matrix must execute locked tests, not only check compilation'
    }
}
$rootCommand = (Get-NativeQualificationCommand 'root-no-default').Arguments -join ' '
if ($rootCommand -notmatch '-p xana .*--no-default-features' -or $rootCommand -match '--workspace') {
    throw 'root no-default must avoid workspace feature unification'
}
foreach ($package in @('xana', 'xana-desktop')) {
    $command = Get-NativeQualificationCommand "build-$package"
    if ($command.Program -ne 'cargo' -or
        ($command.Arguments -join ' ') -cne "build --locked --release -p $package") {
        throw 'native executable sizes require separate locked package builds without Desktop feature unification'
    }
}

# Exercise the production build/report loop, not only its command catalog.
# Stubs are local to this scope: no Cargo invocation or fake binary is needed.
& {
    . "$PSScriptRoot/native-qualification.ps1"
    $builds = [Collections.Generic.List[string]]::new()
    function Invoke-NativeQualificationCommand {
        param($Program, $Arguments, $LogPath)
        $builds.Add("$Program $($Arguments -join ' ')")
        Write-Output "Compiling synthetic package; log: $LogPath"
    }
    function Get-Item { param($LiteralPath) return @{ Length = 123 } }
    function Get-FileHash { param($LiteralPath, $Algorithm) return @{ Hash = ('A' * 64) } }
    $records = @(Measure-NativePackages 'fixture-logs')
    if ($records.Count -ne 2 -or $records[0].name -ne 'xana' -or
        $records[1].name -ne 'xana-desktop') {
        throw 'package evidence must contain only two binary records, never build output'
    }
    foreach ($index in 0..1) {
        $record = $records[$index]
        if ($record.bytes -ne 123 -or $record.sha256 -cne ('a' * 64) -or
            ($record.command -join ' ') -cne $builds[$index]) {
            throw 'package evidence lost its exact command, size or checksum'
        }
    }
}

# Exercise real subprocess failure/log propagation, without running Cargo,
# opening any keychain or needing any network connection.
$log = [IO.Path]::GetTempFileName()
try {
    Invoke-NativeQualificationCommand pwsh @('-NoProfile', '-Command', "Write-Output 'synthetic pass'; exit 0") $log
    if ((Get-Content -Raw -LiteralPath $log) -notmatch 'synthetic pass') { throw 'success log missing' }
    Assert-Rejected {
        Invoke-NativeQualificationCommand pwsh @('-NoProfile', '-Command', "Write-Output 'synthetic failure'; exit 23") $log
    }
    if ((Get-Content -Raw -LiteralPath $log) -notmatch 'synthetic failure') { throw 'failure log missing' }
} finally {
    Remove-Item -LiteralPath $log -Force
}

function Assert-NativeWorkflow {
    param([string]$Workflow)
    $triggers = [regex]::Match($Workflow, '(?ms)^on:\r?\n(.*?)(?=^\S)').Groups[1].Value.Trim()
    if ($triggers -ne 'workflow_dispatch:') { throw 'native qualification must remain manual-only' }
    if ($Workflow -match '(?m)^\s+(?:push|pull_request|schedule|workflow_call):|: write\s*$|\$\{\{\s*secrets\.|continue-on-error:|self-hosted') {
        throw 'qualification may neither expand authority nor mask failure'
    }
    $uses = [regex]::Matches($Workflow, '(?m)^\s*uses:\s*([^@\s]+)@([^\s#]+)')
    if ($uses.Count -ne 4) { throw 'unexpected qualification action surface' }
    foreach ($use in $uses) {
        if ($use.Groups[2].Value -cnotmatch '^[0-9a-f]{40}$') { throw 'unpinned qualification action' }
    }
    foreach ($required in @(
        'contents: read', 'persist-credentials: false', 'save-if: false', 'fail-fast: false',
        'ubuntu-24.04', 'macos-15', 'macos-15-intel', 'aarch64-apple-darwin',
        'x86_64-apple-darwin', 'x86_64-unknown-linux-gnu',
        'timeout-minutes: ${{ matrix.job_minutes }}', 'timeout-minutes: ${{ matrix.resource_minutes }}',
        'timeout-minutes: 15', 'if: always()', 'retention-days: 7',
        'target/native-qualification/*.log', 'target/native-qualification/*.json'
        'libfontconfig1-dev', 'libxkbcommon-dev', 'libxkbcommon-x11-dev', 'libxcb1-dev'
    )) {
        if (-not $Workflow.Contains($required)) { throw "missing native workflow contract: $required" }
    }
    # These are cold-build allowances, not history-probe or application SLAs.
    # Intel exhausted 90 minutes after successful history probes while compiling
    # Desktop. Keep the proven Linux/ARM limits and bounded serialized builds.
    $normalized = $Workflow.Replace("`r`n", "`n")
    foreach ($policy in @(
        @('ubuntu-24.04', 'x86_64-unknown-linux-gnu', 180, 90),
        @('macos-15', 'aarch64-apple-darwin', 180, 90),
        @('macos-15-intel', 'x86_64-apple-darwin', 300, 150)
    )) {
        $row = "          - runner: $($policy[0])`n            target: $($policy[1])`n            job_minutes: $($policy[2])`n            resource_minutes: $($policy[3])"
        if (-not $normalized.Contains($row)) { throw 'native target build allowance is missing or changed' }
    }
    if (-not $Workflow.Contains('CARGO_BUILD_JOBS: "1"')) { throw 'native builds must retain serialized memory pressure' }
    foreach ($check in @('contracts', 'format', 'lint', 'all-features', 'no-default', 'root-no-default', 'custody', 'source-package', 'resources')) {
        if (-not $Workflow.Contains("run-native-qualification.ps1 -Check $check")) { throw "missing native gate: $check" }
    }
    $paths = [regex]::Match($Workflow, '(?ms)^\s{10}path: \|\r?\n(.*?)(?=^\s{10}\S)').Groups[1].Value
    $entries = @($paths.Trim() -split '\r?\n' | ForEach-Object { $_.Trim() })
    if (($entries -join ',') -ne 'target/native-qualification/*.log,target/native-qualification/*.json') {
        throw 'artifact paths must never include fixture homes, credentials or databases'
    }
}
$workflow = Get-Content -Raw -LiteralPath "$PSScriptRoot/../.github/workflows/native-qualification.yml"
Assert-NativeWorkflow $workflow
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'workflow_dispatch:', 'push:') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'contents: read', 'contents: write') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'checkout@[0-9a-f]{40}', 'checkout@main') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace '\*\.json', '**') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace '-Check custody', '-Check format') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace '-Check resources', '-Check format') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'libxkbcommon-x11-dev', '') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'job_minutes: 300', 'job_minutes: 180') }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'resource_minutes: 150', 'resource_minutes: 90') }
Assert-Rejected { Assert-NativeWorkflow ($workflow.Replace('timeout-minutes: ${{ matrix.resource_minutes }}', 'timeout-minutes: 90')) }
Assert-Rejected { Assert-NativeWorkflow ($workflow -replace 'CARGO_BUILD_JOBS: "1"', 'CARGO_BUILD_JOBS: "4"') }

# The production custody script must refuse this synthetic call before touching
# the OS. Use Git Bash explicitly on Windows, never the Windows WSL shim.
$bash = if ($IsWindows) { "$env:ProgramFiles/Git/bin/bash.exe" } else { 'bash' }
if (-not (Get-Command $bash -ErrorAction SilentlyContinue)) { throw 'Bash is required for custody script checks' }
& $bash -n "$PSScriptRoot/qualify-native-custody.sh"
if ($LASTEXITCODE -ne 0) { throw 'native custody Bash syntax failed' }
& $bash "$PSScriptRoot/test-native-custody.sh"
if ($LASTEXITCODE -ne 0) { throw 'native custody shell fixtures failed' }
$savedActions = $env:GITHUB_ACTIONS
try {
    $env:GITHUB_ACTIONS = 'false'
    $PSNativeCommandUseErrorActionPreference = $false
    & $bash "$PSScriptRoot/qualify-native-custody.sh"
    if ($LASTEXITCODE -eq 0) { throw 'custody script allowed a workstation invocation' }
} finally {
    $env:GITHUB_ACTIONS = $savedActions
}
Write-Output 'native qualification verified: manual-only, pinned, exact tests, failure evidence, safe workstation refusal'
