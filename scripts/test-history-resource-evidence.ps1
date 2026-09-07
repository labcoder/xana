$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/history-resource-evidence.ps1"

function Assert-Rejected {
    param([string]$Log)
    try { Get-HistoryResourceEvidence $Log 10000 | Out-Null }
    catch { return }
    throw 'incomplete history evidence was accepted'
}

$trials = 0..4 | ForEach-Object {
    "protected_execution messages=10000 trial=$_ open_us=$($_ + 1) resume_us=$($_ + 10) page_median_us=8 page_p95_us=20 retained_entries=128 execution_bytes=1024 initial_page_bytes=1024"
}
$fixture = @(
    'production_fixture messages=10000 generation_ms=15 journal_bytes=100000'
    $trials
    'immutable_verification messages=10000 elapsed_ms=10'
    'protected_backup messages=10000 elapsed_ms=20'
    'protected_restore messages=10000 elapsed_ms=30'
    "test $HistoryResourceTest ... ok"
    'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s'
) -join "`n"
foreach ($log in @($fixture, $fixture.Replace("`n", "`r`n"), $fixture.Replace("test $HistoryResourceTest ... ok", "test $HistoryResourceTest ... fixture_directory=synthetic`nok"))) {
    $evidence = Get-HistoryResourceEvidence $log 10000
    if ($evidence.trials.Count -ne 5 -or $evidence.open_median_us -ne 3 -or $evidence.open_p95_us -ne 5 -or $evidence.restore_ms -ne 30) {
        throw 'complete evidence or nearest-rank five-sample summaries changed'
    }
}
Assert-Rejected ($fixture -replace 'trial=4', 'trial=3')
Assert-Rejected ($fixture -replace 'trial=4', 'trial=5')
Assert-Rejected ($fixture -replace 'protected_restore', 'skipped_restore')
Assert-Rejected ($fixture -replace 'immutable_verification', 'skipped_verification')
Assert-Rejected ($fixture -replace 'protected_backup', 'skipped_backup')
Assert-Rejected ($fixture -replace [regex]::Escape($HistoryResourceTest), 'wrong_test')
Assert-Rejected ($fixture -replace '1 passed; 0 failed; 0 ignored;', '0 passed; 0 failed; 1 ignored;')
Assert-Rejected ($fixture -replace 'messages=10000 trial=4', 'messages=100000 trial=4')
Assert-Rejected ($fixture -replace 'page_p95_us=20', 'page_p95_us=NaN')
Assert-Rejected "$fixture`nverification_profile_only=true backup_restore_not_measured=true"
Assert-Rejected "$fixture`n$fixture"
Assert-Rejected "$fixture`ntest result: FAILED. 0 passed; 1 failed; 0 ignored;"
Assert-Rejected ($fixture + ('x' * 131073))
Write-Output 'history evidence verified: exact test, five distinct trials, LF/CRLF, complete verification/backup/restore, preserved failures'
