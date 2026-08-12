[CmdletBinding()]
param(
    [string]$Repository = $env:GITHUB_REPOSITORY,
    [string]$Commit = $env:GITHUB_SHA,
    [string]$Branch = "main",
    [string]$Workflow = "ci.yml"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. "$PSScriptRoot/release-ci-evidence.ps1"

if ($Repository -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
    throw "GITHUB_REPOSITORY must be an owner/repository name"
}
if ($Commit -cnotmatch '^[0-9a-fA-F]{40}$') {
    throw "GITHUB_SHA must be a full commit SHA"
}
if ($Branch -cne "main" -or $Workflow -cne "ci.yml") {
    throw "release evidence is fixed to the main branch and CI workflow"
}
if ([string]::IsNullOrWhiteSpace($env:GH_TOKEN)) {
    throw "GH_TOKEN is required to inspect CI evidence"
}

$responseJson = (& gh api --method GET `
    -H "Accept: application/vnd.github+json" `
    -H "X-GitHub-Api-Version: 2022-11-28" `
    "repos/$Repository/actions/workflows/$Workflow/runs" `
    -f "head_sha=$Commit" `
    -f "branch=$Branch" `
    -f "event=push" `
    -f "status=success" `
    -f "exclude_pull_requests=true" `
    -f "per_page=20" | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw "could not query exact CI evidence"
}
if ([Text.Encoding]::UTF8.GetByteCount($responseJson) -gt 4MB) {
    throw "CI evidence response exceeded its bounded size"
}

try {
    $response = $responseJson | ConvertFrom-Json
} catch {
    throw "CI evidence response was not valid JSON"
}
$run = Select-ReleaseCiEvidence `
    -Response $response `
    -Commit $Commit `
    -Branch $Branch `
    -Repository $Repository

Write-Output "release source verified by successful CI run $($run.id): $($run.html_url)"
