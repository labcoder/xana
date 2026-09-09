# Expensive, explicit native qualification; no model, account or real data home.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/native-qualification.ps1"
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    $output = 'target/native-qualification'
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    Invoke-NativeQualificationCommand cargo @('test', '--locked', '--release', '-p', 'xana', '--lib', '--all-features', '--no-run', '--message-format=json') "$output/resource-build.log"
    $executables = @(Get-Content -LiteralPath "$output/resource-build.log" | ForEach-Object {
        if ($_.StartsWith('{')) {
            $artifact = $_ | ConvertFrom-Json
            if ($artifact.reason -eq 'compiler-artifact' -and $artifact.target.name -eq 'xana' -and
                'lib' -in $artifact.target.kind -and $artifact.profile.test -and $artifact.executable) {
                if ($artifact.profile.debug_assertions -or $artifact.profile.opt_level -ne '3') {
                    throw 'history resource qualification needs the unchanged optimized release profile'
                }
                $artifact.executable
            }
        }
    })
    if ($executables.Count -ne 1) { throw 'could not identify one exact optimized Xana library test executable' }
    # The measurement script owns each process, joined failure cleanup and report.
    foreach ($count in @(10000, 100000)) {
        & "$PSScriptRoot/measure-history-resources.ps1" -TestExecutable $executables[0] -Messages $count -OutputDirectory $output
    }
    $suffix = if ($IsWindows) { '.exe' } else { '' }
    $binaries = foreach ($package in @('xana', 'xana-desktop')) {
        # Separate Cargo invocations avoid a workspace-wide feature union in
        # the CLI measurement. Record each executable immediately after its build.
        $command = Get-NativeQualificationCommand "build-$package"
        Invoke-NativeQualificationCommand $command.Program $command.Arguments "$output/resource-$package-build.log"
        $path = "target/release/$package$suffix"
        [ordered]@{
            name = $package; command = @($command.Program) + $command.Arguments
            bytes = (Get-Item -LiteralPath $path).Length
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    [ordered]@{
        status = 'passed'; profile = 'release'; binaries = @($binaries)
        limitations = @('Uncompressed native executable sizes, not installer/archive sizes', 'Desktop is built, not launched; GUI/idle/FPS acceptance is separate')
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$output/resource-packages.json" -Encoding utf8
} finally { Pop-Location }
