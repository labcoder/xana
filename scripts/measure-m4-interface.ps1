#!/usr/bin/env pwsh

[CmdletBinding()]
param(
    [string]$OutputPath = "target/m4-interface-evidence.md"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-MeasurementProbe {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    Write-Host "==> $Name"
    $started = [Diagnostics.Stopwatch]::StartNew()
    $lines = @(& cargo @Arguments 2>&1 | ForEach-Object { $_.ToString() })
    $exitCode = $LASTEXITCODE
    $started.Stop()
    if ($exitCode -ne 0) {
        throw "$Name failed with exit code $exitCode`n$($lines -join "`n")"
    }
    [PSCustomObject]@{
        Name = $Name
        ElapsedMilliseconds = $started.ElapsedMilliseconds
        Output = $lines -join "`n"
    }
}

function Resolve-OutputPath {
    param([Parameter(Mandatory = $true)][string]$Repository)

    $candidate = if ([IO.Path]::IsPathRooted($OutputPath)) {
        [IO.Path]::GetFullPath($OutputPath)
    } else {
        [IO.Path]::GetFullPath((Join-Path $Repository $OutputPath))
    }
    $targetRoot = [IO.Path]::GetFullPath((Join-Path $Repository "target"))
    $targetPrefix = $targetRoot.TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    ) + [IO.Path]::DirectorySeparatorChar
    $pathComparison = if ($IsWindows) {
        [StringComparison]::OrdinalIgnoreCase
    } else {
        [StringComparison]::Ordinal
    }
    if (-not $candidate.StartsWith($targetPrefix, $pathComparison)) {
        throw "M4 measurement output must remain under $targetRoot"
    }
    $candidate
}

function Read-ProbeMetric {
    param([Parameter(Mandatory = $true)]$Probe)

    $prefix = "m4_metric "
    $line = @($Probe.Output -split "`r?`n" | Where-Object { $_.StartsWith($prefix) })
    if ($line.Count -ne 1) {
        throw "$($Probe.Name) emitted $($line.Count) machine-readable metric records; expected exactly one"
    }
    $line[0].Substring($prefix.Length) | ConvertFrom-Json
}

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$resolvedOutput = Resolve-OutputPath -Repository $repository
$outputDirectory = Split-Path -Parent $resolvedOutput
$priorIncremental = $env:CARGO_INCREMENTAL

Push-Location $repository
try {
    $env:CARGO_INCREMENTAL = "0"
    $build = Invoke-MeasurementProbe -Name "Release interface binaries" -Arguments @(
        "build", "--locked", "--release", "-p", "xana", "-p", "xana-desktop"
    )
    $snapshot = Invoke-MeasurementProbe -Name "Bounded 10,000-message snapshot projection" -Arguments @(
        "test", "--locked", "--release", "--lib",
        "frontend::protocol::tests::m4_reference_snapshot_projection_probe",
        "--", "--ignored", "--exact", "--nocapture"
    )
    $desktop = Invoke-MeasurementProbe -Name "Bounded Desktop progressive projection" -Arguments @(
        "test", "--locked", "--release", "-p", "xana-desktop",
        "projection::tests::m4_reference_desktop_projection_probe",
        "--", "--ignored", "--exact", "--nocapture"
    )
    $snapshotMetric = Read-ProbeMetric -Probe $snapshot
    $desktopMetric = Read-ProbeMetric -Probe $desktop

    New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
    $runtime = [Runtime.InteropServices.RuntimeInformation]
    $rust = (& rustc --version | Out-String).Trim()
    $cargo = (& cargo --version | Out-String).Trim()
    $recordedAt = [DateTime]::UtcNow.ToString("o")
    $metadata = (& cargo metadata --locked --format-version 1 --no-deps | ConvertFrom-Json)
    $binarySuffix = if ($IsWindows) { ".exe" } else { "" }
    $cliBinary = Get-Item -LiteralPath (Join-Path $metadata.target_directory "release/xana$binarySuffix")
    $desktopBinary = Get-Item -LiteralPath (Join-Path $metadata.target_directory "release/xana-desktop$binarySuffix")
    $jsonOutput = [IO.Path]::ChangeExtension($resolvedOutput, ".json")
    $machineRecord = [ordered]@{
        schema_version = 1
        recorded_at_utc = $recordedAt
        os = $runtime::OSDescription
        architecture = $runtime::OSArchitecture.ToString()
        dotnet_runtime = $runtime::FrameworkDescription
        rust = $rust
        cargo = $cargo
        profile = "release"
        binary_bytes = [ordered]@{
            cli_tui = $cliBinary.Length
            desktop = $desktopBinary.Length
        }
        probes = @($snapshotMetric, $desktopMetric)
        excluded_measurements = @(
            "gpui_paint", "input", "startup", "frame", "cpu", "memory",
            "gpu", "display_refresh", "assistive_technology"
        )
    }
    $machineRecord | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $jsonOutput -Encoding utf8
    $record = @"
# Xana M4 local interface measurement

- Recorded UTC: $recordedAt
- OS: $($runtime::OSDescription)
- Architecture: $($runtime::OSArchitecture)
- Runtime: $($runtime::FrameworkDescription)
- Rust: $rust
- Cargo: $cargo
- Profile: release; incremental compilation disabled
- Machine-readable record: $jsonOutput
- CLI/TUI binary: $($cliBinary.Length) bytes
- Desktop binary: $($desktopBinary.Length) bytes

This file records deterministic data-plane probes, not GPUI paint, input,
startup, frame, CPU, memory, GPU, display-refresh, or assistive-technology
evidence. Complete those owner/reference-system fields in the M4 evidence
matrix rather than inferring them from these timings.

## $($snapshot.Name)

- Command elapsed: $($snapshot.ElapsedMilliseconds) ms (includes test-process startup)

````text
$($snapshot.Output)
````

## $($desktop.Name)

- Command elapsed: $($desktop.ElapsedMilliseconds) ms (includes test-process startup)

````text
$($desktop.Output)
````

## Build transcript

- Command elapsed: $($build.ElapsedMilliseconds) ms

````text
$($build.Output)
````
"@
    Set-Content -LiteralPath $resolvedOutput -Value $record -Encoding utf8
    Write-Host "M4 local measurements written to $resolvedOutput and $jsonOutput"
}
finally {
    if ($null -eq $priorIncremental) {
        Remove-Item Env:CARGO_INCREMENTAL -ErrorAction SilentlyContinue
    } else {
        $env:CARGO_INCREMENTAL = $priorIncremental
    }
    Pop-Location
}
