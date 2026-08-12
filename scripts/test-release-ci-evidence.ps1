$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. "$PSScriptRoot/release-ci-evidence.ps1"

$commit = "0123456789abcdef0123456789abcdef01234567"
$repository = "labcoder/xana"

function New-FixtureRun {
    param(
        [string]$HeadSha = $commit,
        [string]$HeadBranch = "main",
        [string]$Event = "push",
        [string]$Status = "completed",
        [string]$Conclusion = "success",
        [string]$HeadRepository = $repository,
        [int64]$Id = 42,
        [int]$RunNumber = 7
    )

    return [pscustomobject]@{
        id = $Id
        run_number = $RunNumber
        html_url = "https://github.com/labcoder/xana/actions/runs/$Id"
        head_sha = $HeadSha
        head_branch = $HeadBranch
        event = $Event
        status = $Status
        conclusion = $Conclusion
        head_repository = [pscustomobject]@{ full_name = $HeadRepository }
    }
}

function Assert-Rejected {
    param([Parameter(Mandatory = $true)][object[]]$Runs)

    $rejected = $false
    try {
        Select-ReleaseCiEvidence `
            -Response ([pscustomobject]@{ workflow_runs = $Runs }) `
            -Commit $commit `
            -Branch "main" `
            -Repository $repository *> $null
    } catch {
        $rejected = $true
    }
    if (-not $rejected) { throw "invalid CI evidence was accepted" }
}

$valid = New-FixtureRun
$selected = Select-ReleaseCiEvidence `
    -Response ([pscustomobject]@{ workflow_runs = @($valid) }) `
    -Commit $commit `
    -Branch "main" `
    -Repository $repository
if ($selected.id -ne 42) { throw "valid exact CI evidence was not selected" }

Assert-Rejected -Runs @((New-FixtureRun -HeadSha ("f" * 40)))
Assert-Rejected -Runs @((New-FixtureRun -HeadBranch "feature"))
Assert-Rejected -Runs @((New-FixtureRun -Event "pull_request"))
Assert-Rejected -Runs @((New-FixtureRun -Conclusion "failure"))
Assert-Rejected -Runs @((New-FixtureRun -Status "in_progress"))
Assert-Rejected -Runs @((New-FixtureRun -HeadRepository "someone/fork"))

$newer = New-FixtureRun -Id 84 -RunNumber 9
$older = New-FixtureRun -Id 21 -RunNumber 3
$selected = Select-ReleaseCiEvidence `
    -Response ([pscustomobject]@{ workflow_runs = @(
        (New-FixtureRun -Conclusion "failure" -RunNumber 10),
        $older,
        $newer
    ) }) `
    -Commit $commit `
    -Branch "main" `
    -Repository $repository
if ($selected.id -ne 84) { throw "newest exact successful CI evidence was not selected" }

Write-Output "release CI evidence verified: exact commit, branch, event, repository, and success required"
