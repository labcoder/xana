param(
    [Parameter(Mandatory = $true)][string]$TestExecutable,
    [ValidateSet(10000, 100000)][int]$Messages = 10000
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This process sampler currently records Windows evidence only; native Mac/Linux measurement remains a separate verification gate.'
}

# The caller builds once at the coordinated release-validation barrier. This
# runner measures a fresh fixture process; it never opens the user's Xana home.
$taskBinary = (Resolve-Path -LiteralPath $TestExecutable).Path
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskTarget = Join-Path $taskRoot 'target'
if (-not $taskBinary.StartsWith($taskTarget + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The test executable must be inside this checkout target directory.'
}
$taskLogs = Join-Path (Join-Path $taskTarget 'history-probes') ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $taskLogs -Force | Out-Null
$taskStdout = Join-Path $taskLogs 'stdout.txt'
$taskStderr = Join-Path $taskLogs 'stderr.txt'
$taskPriorCount = [Environment]::GetEnvironmentVariable('XANA_HISTORY_PROBE_MESSAGES', 'Process')
$taskProcess = $null
try {
    [Environment]::SetEnvironmentVariable('XANA_HISTORY_PROBE_MESSAGES', "$Messages", 'Process')
    $taskProcess = Start-Process -FilePath $taskBinary -ArgumentList @(
        'storage::history::execution_tests::protected_execution_resource_probe',
        '--exact', '--ignored', '--nocapture', '--test-threads=1'
    ) -WorkingDirectory $taskRoot -WindowStyle Hidden -PassThru -RedirectStandardOutput $taskStdout -RedirectStandardError $taskStderr
    $taskTimer = [Diagnostics.Stopwatch]::StartNew()
    $taskPeakRss = 0L
    $taskCpuSeconds = 0.0
    while (-not $taskProcess.HasExited) {
        $taskProcess.Refresh()
        if (-not $taskProcess.HasExited) {
            $taskPeakRss = [Math]::Max($taskPeakRss, $taskProcess.PeakWorkingSet64)
            $taskCpuSeconds = [Math]::Max($taskCpuSeconds, $taskProcess.TotalProcessorTime.TotalSeconds)
        }
        if ($taskTimer.Elapsed.TotalMinutes -gt 30) {
            $taskProcess.Kill()
            throw 'The isolated history resource probe exceeded 30 minutes.'
        }
        Start-Sleep -Milliseconds 100
    }
    $taskProcess.WaitForExit()
    $taskTimer.Stop()
    $taskOutput = Get-Content -LiteralPath $taskStdout -Raw
    Write-Output $taskOutput
    $taskError = Get-Content -LiteralPath $taskStderr -Raw
    if ($taskError) { Write-Output $taskError }
    if ($taskProcess.ExitCode -ne 0 -or $taskOutput -notmatch "production_fixture messages=$Messages ") {
        throw "The isolated history probe did not pass; logs: $taskLogs"
    }
    $taskProfileOnly = $taskOutput.Contains('verification_profile_only=true backup_restore_not_measured=true')
    if (-not $taskProfileOnly -and $taskOutput -notmatch "protected_restore messages=$Messages ") {
        throw "The history probe omitted required restore evidence; logs: $taskLogs"
    }
    [ordered]@{
        scenario = 'protected-backend-test-process-not-gui'
        complete_resource_probe = -not $taskProfileOnly
        messages = $Messages
        peak_rss_bytes = $taskPeakRss
        sampled_cpu_seconds = $taskCpuSeconds
        wall_seconds = $taskTimer.Elapsed.TotalSeconds
        logical_processors = [Environment]::ProcessorCount
        logs = $taskLogs
    } | ConvertTo-Json
}
finally {
    [Environment]::SetEnvironmentVariable('XANA_HISTORY_PROBE_MESSAGES', $taskPriorCount, 'Process')
    if ($taskProcess) { $taskProcess.Dispose() }
}
