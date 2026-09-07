# Parsing is shared by the native sampler and offline negative fixtures.
Set-StrictMode -Version Latest
$HistoryResourceTest = 'storage::history::execution_tests::protected_execution_resource_probe'

function Get-HistoryResourceEvidence {
    param([Parameter(Mandatory)][string]$Log, [ValidateSet(10000, 100000)][int]$Messages)
    if ($Log.Length -gt 131072 -or $Log.Contains('verification_profile_only=true')) {
        throw 'history qualification requires a bounded complete probe, including backup/restore'
    }
    # libtest --nocapture may put the first fixture line after `... ` and
    # print `ok` later. One exact selected test plus its summary is the proof.
    $testLine = '(?m)^test ' + [regex]::Escape($HistoryResourceTest) + ' \.\.\. '
    if ([regex]::Matches($Log, $testLine).Count -ne 1 -or
        [regex]::Matches($Log, 'test result: ok\. 1 passed; 0 failed; 0 ignored;').Count -ne 1 -or
        $Log -match 'test result: FAILED\.') {
        throw 'the exact history resource test must pass once without ignored tests'
    }
    $generation = [regex]::Matches($Log, "(?m)^production_fixture messages=$Messages generation_ms=(\d+) journal_bytes=(\d+)\r?$")
    $backup = [regex]::Matches($Log, "(?m)^protected_backup messages=$Messages elapsed_ms=(\d+)\r?$")
    $restore = [regex]::Matches($Log, "(?m)^protected_restore messages=$Messages elapsed_ms=(\d+)\r?$")
    $verification = [regex]::Matches($Log, "(?m)^immutable_verification messages=$Messages elapsed_ms=(\d+)\r?$")
    if ($generation.Count -ne 1 -or $backup.Count -ne 1 -or $restore.Count -ne 1 -or $verification.Count -ne 1) {
        throw 'history qualification omitted or duplicated generation, verification, backup or restore'
    }
    $pattern = "(?m)^protected_execution messages=$Messages trial=(\d+) open_us=(\d+) resume_us=(\d+) page_median_us=(\d+) page_p95_us=(\d+) retained_entries=(\d+) execution_bytes=(\d+) initial_page_bytes=(\d+)\r?$"
    $matches = [regex]::Matches($Log, $pattern)
    if ($matches.Count -ne 5 -or [regex]::Matches($Log, '(?m)^protected_execution ').Count -ne 5) {
        throw 'history qualification requires all five exact-size trials'
    }
    $trials = @($matches | ForEach-Object {
        [ordered]@{
            trial = [int]$_.Groups[1].Value; open_us = [long]$_.Groups[2].Value
            resume_us = [long]$_.Groups[3].Value; page_median_us = [long]$_.Groups[4].Value
            page_p95_us = [long]$_.Groups[5].Value; retained_entries = [long]$_.Groups[6].Value
            execution_bytes = [long]$_.Groups[7].Value; initial_page_bytes = [long]$_.Groups[8].Value
        }
    })
    if ((($trials | ForEach-Object { $_.trial } | Sort-Object) -join ',') -ne '0,1,2,3,4') {
        throw 'history qualification trial identities must be unique and complete'
    }
    $opens = @($trials | ForEach-Object { $_.open_us } | Sort-Object)
    $resumes = @($trials | ForEach-Object { $_.resume_us } | Sort-Object)
    return [ordered]@{
        messages = $Messages; trials = $trials; generation_ms = [long]$generation[0].Groups[1].Value
        journal_bytes = [long]$generation[0].Groups[2].Value
        verification_ms = [long]$verification[0].Groups[1].Value
        backup_ms = [long]$backup[0].Groups[1].Value; restore_ms = [long]$restore[0].Groups[1].Value
        open_median_us = $opens[2]; open_p95_us = $opens[4]
        resume_median_us = $resumes[2]; resume_p95_us = $resumes[4]
        measurement = 'Five independent database opens in one fresh synthetic test process; not OS-cold caches or GUI latency. Five-sample nearest-rank p95 is the maximum.'
    }
}
