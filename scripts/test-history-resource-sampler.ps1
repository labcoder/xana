$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$fixtureRoot = Join-Path $root ('target/sampler-fixtures/' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
$suffix = if ($IsWindows) { '.exe' } else { '' }
$binary = Join-Path $fixtureRoot "worker$suffix"
& rustc --edition 2024 (Join-Path $root 'tests/fixtures/history-resource-worker.rs') -o $binary
if ($LASTEXITCODE -ne 0) { throw 'sampler fixture compilation failed' }
$priorMode = $env:XANA_TEST_HISTORY_WORKER_MODE
$priorProfile = $env:XANA_HISTORY_VERIFY_PROFILE_ONLY
try {
    $env:XANA_HISTORY_VERIFY_PROFILE_ONLY = 'must-not-reach-child'
    $env:XANA_TEST_HISTORY_WORKER_MODE = 'normal'
    & "$PSScriptRoot/measure-history-resources.ps1" -TestExecutable $binary -Messages 10000 -OutputDirectory $fixtureRoot | Out-Null
    $report = Get-Content -Raw -LiteralPath (Join-Path $fixtureRoot 'history-10000.json') | ConvertFrom-Json
    if ($report.status -ne 'passed' -or -not $report.complete_resource_probe -or -not $report.cleanup_confirmed -or $report.samples -le 0) {
        throw 'sampler omitted successful process evidence'
    }
    $env:XANA_TEST_HISTORY_WORKER_MODE = 'hang'
    $rejected = $false
    try {
        & "$PSScriptRoot/measure-history-resources.ps1" -TestExecutable $binary -Messages 10000 -OutputDirectory $fixtureRoot -TimeoutSeconds 1 | Out-Null
    } catch { $rejected = $true }
    $report = Get-Content -Raw -LiteralPath (Join-Path $fixtureRoot 'history-10000.json') | ConvertFrom-Json
    if (-not $rejected -or $report.status -ne 'failed' -or $report.complete_resource_probe -or -not $report.cleanup_confirmed) {
        throw 'sampler timeout must fail and retain confirmed process cleanup'
    }
    $pidLine = Get-Content -LiteralPath (Join-Path $fixtureRoot 'history-10000.log') | Select-String '^fixture_pid=(\d+)$'
    if (-not $pidLine -or (Get-Process -Id ([int]$pidLine.Matches[0].Groups[1].Value) -ErrorAction SilentlyContinue)) {
        throw 'sampler abandoned its owned test process'
    }
    if ($env:XANA_HISTORY_VERIFY_PROFILE_ONLY -ne 'must-not-reach-child') { throw 'sampler changed the caller environment' }
} finally {
    $env:XANA_TEST_HISTORY_WORKER_MODE = $priorMode
    $env:XANA_HISTORY_VERIFY_PROFILE_ONLY = $priorProfile
}
Write-Output 'sampler fixtures passed: literal argv, isolated options, resource samples, joined timeout, failed report; not native history qualification'
