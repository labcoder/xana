# Shared by the manual workflow driver and its offline failure fixtures.
Set-StrictMode -Version Latest

$NativeCustodyTest = 'storage::keys::native_tests::production_os_custody_unlock_loss_lock_and_independent_recovery'

function Get-NativeQualificationCommand {
    param([Parameter(Mandatory)][string]$Check)
    $test = @('test', '--locked')
    switch ($Check) {
        'format' { return @{ Program = 'cargo'; Arguments = @('fmt', '--all', '--check') } }
        'contracts' { return @{ Program = 'pwsh'; Arguments = @('-NoProfile', '-File', 'scripts/test-native-qualification.ps1') } }
        'lint' { return @{ Program = 'cargo'; Arguments = @('clippy', '--locked', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings') } }
        'all-features' { return @{ Program = 'cargo'; Arguments = $test + @('--workspace', '--all-targets', '--all-features') } }
        'no-default' { return @{ Program = 'cargo'; Arguments = $test + @('--workspace', '--all-targets', '--no-default-features') } }
        'root-no-default' { return @{ Program = 'cargo'; Arguments = $test + @('-p', 'xana', '--all-targets', '--no-default-features') } }
        'custody' { return @{ Program = 'bash'; Arguments = @('scripts/qualify-native-custody.sh') } }
        'resources' { return @{ Program = 'pwsh'; Arguments = @('-NoProfile', '-File', 'scripts/qualify-native-resources.ps1') } }
        default { throw "unknown native qualification check: $Check" }
    }
}

function Get-NativeTestEvidence {
    param([Parameter(Mandatory)][string]$Log, [switch]$Custody)
    $totals = @{ passed = 0; failed = 0; ignored = 0 }
    $summaries = [regex]::Matches($Log, 'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;')
    foreach ($summary in $summaries) {
        $totals.passed += [int]$summary.Groups[1].Value
        $totals.failed += [int]$summary.Groups[2].Value
        $totals.ignored += [int]$summary.Groups[3].Value
    }
    if ($totals.passed -eq 0 -or $totals.failed -ne 0) {
        throw 'native qualification needs executed passing tests, not a compile-only or empty selection'
    }
    if ($Custody -and ($totals.passed -ne 1 -or $totals.ignored -ne 0 -or
        $Log -notmatch ('(?m)^test ' + [regex]::Escape($NativeCustodyTest) + ' \.\.\. ok\r?$'))) {
        throw 'the exact production custody fixture did not pass once'
    }
    return $totals
}

function Invoke-NativeQualificationCommand {
    param(
        [Parameter(Mandatory)][string]$Program,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$LogPath
    )
    # Stream output to console/file, not an ever-growing in-memory transcript.
    $PSNativeCommandUseErrorActionPreference = $false
    & $Program @Arguments 2>&1 | Tee-Object -FilePath $LogPath
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE; see $LogPath" }
}
