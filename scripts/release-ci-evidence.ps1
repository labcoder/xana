$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Select-ReleaseCiEvidence {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][object]$Response,
        [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{40}$')][string]$Commit,
        [Parameter(Mandatory = $true)][string]$Branch,
        [Parameter(Mandatory = $true)][string]$Repository
    )

    $runs = @($Response.workflow_runs)
    $matchingRuns = @($runs | Where-Object {
        $_.head_sha -ceq $Commit -and
        $_.head_branch -ceq $Branch -and
        $_.event -ceq "push" -and
        $_.status -ceq "completed" -and
        $_.conclusion -ceq "success" -and
        $_.head_repository.full_name -ceq $Repository
    } | Sort-Object -Property run_number -Descending)

    if ($matchingRuns.Count -eq 0) {
        throw "the exact release commit has no successful main-branch push CI run"
    }
    $selectedRun = $matchingRuns[0]
    if ($selectedRun.id -notmatch '^[0-9]+$' -or
        $selectedRun.html_url -notmatch '^https://github\.com/') {
        throw "the matching CI evidence has invalid identity metadata"
    }

    return $selectedRun
}
