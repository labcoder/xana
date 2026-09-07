param(
    [Parameter(Mandatory = $true)][string]$TestExecutable,
    [ValidateSet(10000, 100000)][int]$Messages = 10000,
    [string]$OutputDirectory,
    [ValidateRange(1, 1800)][int]$TimeoutSeconds = 1800
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/history-resource-evidence.ps1"

# The caller builds once at the coordinated release-validation barrier. This
# runner measures a fresh fixture process; it never opens the user's Xana home.
$taskBinary = (Resolve-Path -LiteralPath $TestExecutable).Path
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskTarget = Join-Path $taskRoot 'target'
$taskComparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
if (-not $taskBinary.StartsWith($taskTarget + [IO.Path]::DirectorySeparatorChar, $taskComparison)) {
    throw 'The test executable must be inside this checkout target directory.'
}
$taskLogs = Join-Path (Join-Path $taskTarget 'history-probes') ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $taskLogs -Force | Out-Null
$taskStdout = Join-Path $taskLogs 'stdout.txt'
$taskStderr = Join-Path $taskLogs 'stderr.txt'
$taskProcess = $null
$taskReport = [ordered]@{
    status = 'failed'; scenario = 'protected-backend-test-process-not-gui'
    complete_resource_probe = $false; cleanup_confirmed = $false; messages = $Messages
    executable_sha256 = (Get-FileHash -LiteralPath $taskBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    sample_interval_ms = 100; samples = 0; sampled_peak_rss_bytes = 0L
    sampled_cpu_seconds = 0.0; wall_seconds = 0.0
    os = [Runtime.InteropServices.RuntimeInformation]::OSDescription
    architecture = "$( [Runtime.InteropServices.RuntimeInformation]::OSArchitecture )"
    logical_processors = [Environment]::ProcessorCount
    limitations = @('Sampled current working set, not guaranteed peak RSS', 'CPU last sampled before exit, not final process total', 'Includes fixture generation, verification and backup/restore; not GUI or idle measurements')
}
$taskTimer = [Diagnostics.Stopwatch]::new()
try {
    $start = @{
        FilePath = $taskBinary; ArgumentList = @(
        $HistoryResourceTest,
        '--exact', '--ignored', '--nocapture', '--test-threads=1'
        ); WorkingDirectory = $taskRoot; PassThru = $true
        RedirectStandardOutput = $taskStdout; RedirectStandardError = $taskStderr
        Environment = @{ XANA_HISTORY_PROBE_MESSAGES = "$Messages"; XANA_HISTORY_VERIFY_PROFILE_ONLY = $null }
    }
    if ($IsWindows) { $start.WindowStyle = 'Hidden' }
    $taskProcess = Start-Process @start
    $taskTimer.Start()
    while (-not $taskProcess.HasExited) {
        $taskProcess.Refresh()
        try {
            if (-not $taskProcess.HasExited) {
                $taskReport.sampled_peak_rss_bytes = [Math]::Max($taskReport.sampled_peak_rss_bytes, $taskProcess.WorkingSet64)
                $taskReport.sampled_cpu_seconds = [Math]::Max($taskReport.sampled_cpu_seconds, $taskProcess.TotalProcessorTime.TotalSeconds)
                $taskReport.samples++
            }
        } catch {
            # Exit may race a native /proc or task-info query. Other failures
            # must remain failures, not a fabricated zero-resource sample.
            if (-not $taskProcess.HasExited) { throw }
        }
        if ($taskTimer.Elapsed.TotalSeconds -gt $TimeoutSeconds) {
            throw 'The isolated history resource probe exceeded its deadline.'
        }
        if ((Get-Item -LiteralPath $taskStdout).Length -gt 131072 -or (Get-Item -LiteralPath $taskStderr).Length -gt 131072) {
            throw 'history fixture output exceeded its evidence bound'
        }
        Start-Sleep -Milliseconds 100
    }
    $taskProcess.WaitForExit()
    $taskTimer.Stop()
    if ((Get-Item -LiteralPath $taskStdout).Length -gt 131072 -or (Get-Item -LiteralPath $taskStderr).Length -gt 131072) {
        throw 'history fixture output exceeded its evidence bound'
    }
    $taskOutput = Get-Content -LiteralPath $taskStdout -Raw
    Write-Output $taskOutput
    $taskError = Get-Content -LiteralPath $taskStderr -Raw
    if ($taskError) { Write-Output $taskError }
    if ($taskProcess.ExitCode -ne 0 -or $taskReport.samples -eq 0 -or $taskReport.sampled_peak_rss_bytes -le 0) {
        throw "The isolated history probe did not pass; logs: $taskLogs"
    }
    $taskReport['evidence'] = Get-HistoryResourceEvidence -Log $taskOutput -Messages $Messages
    $taskReport.complete_resource_probe = $true
    $taskReport.status = 'passed'
}
finally {
    $cleanupError = $null
    if ($taskProcess) {
        try {
            if (-not $taskProcess.HasExited) {
                $taskProcess.Kill()
                if (-not $taskProcess.WaitForExit(5000)) { throw 'history fixture cleanup was not acknowledged' }
            }
            $taskReport.cleanup_confirmed = $true
        } catch {
            $cleanupError = $_
            $taskReport.status = 'cleanup_failed'
            $taskReport.complete_resource_probe = $false
        } finally {
            $taskProcess.Dispose()
        }
    }
    $taskTimer.Stop()
    $taskReport.wall_seconds = $taskTimer.Elapsed.TotalSeconds
    $taskReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $taskLogs 'report.json') -Encoding utf8
    if ($OutputDirectory) {
        # Only sanitized fixture logs/metadata, never the temporary data home.
        if (Test-Path -LiteralPath $taskStdout) { Copy-Item -LiteralPath $taskStdout -Destination (Join-Path $OutputDirectory "history-$Messages.log") }
        if (Test-Path -LiteralPath $taskStderr) { Copy-Item -LiteralPath $taskStderr -Destination (Join-Path $OutputDirectory "history-$Messages-stderr.log") }
        Copy-Item -LiteralPath (Join-Path $taskLogs 'report.json') -Destination (Join-Path $OutputDirectory "history-$Messages.json")
    }
    $taskReport | ConvertTo-Json -Depth 8
    if ($cleanupError) { throw $cleanupError }
}
