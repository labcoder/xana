$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$path = ".github/workflows/release.yml"
$workflow = Get-Content -Raw -LiteralPath $path
$draftScript = Get-Content -Raw -LiteralPath "scripts/create-release-draft.ps1"
$uses = @([regex]::Matches($workflow, '(?m)^\s*uses:\s*([^@\s]+)@([^\s#]+)'))
if ($uses.Count -eq 0) { throw "release workflow has no pinned actions" }
foreach ($use in $uses) {
    $reference = $use.Groups[2].Value
    if ($reference -cnotmatch '^[0-9a-f]{40}$') {
        throw "release workflow action is not pinned to a full commit: $($use.Value.Trim())"
    }
}

$required = @(
    "ubuntu-24.04",
    "macos-15",
    "macos-15-intel",
    "windows-latest",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "persist-credentials: false",
    "attestations: write",
    "id-token: write",
    "contents: write",
    "github.event_name == 'push'",
    "manual no-publish builds must use the reviewed main branch",
    "verify-sha256.sh",
    "check-release-plan.ps1",
    "assemble-release-bundle.ps1"
)
foreach ($text in $required) {
    if (-not $workflow.Contains($text)) { throw "release workflow is missing required contract: $text" }
}
if (-not $draftScript.Contains("--draft") -or
    -not $draftScript.Contains("refusing to modify an already published release")) {
    throw "draft helper does not preserve the unpublished human-review boundary"
}

$draftJobMatch = [regex]::Match(
    $workflow,
    '(?ms)^  draft:\s*$.*?(?=^  [a-zA-Z0-9_-]+:\s*$|\z)'
)
if (-not $draftJobMatch.Success) {
    throw "release workflow has no draft job"
}
$draftJob = $draftJobMatch.Value
$draftScriptIndex = $draftJob.IndexOf(
    "./scripts/create-release-draft.ps1",
    [StringComparison]::Ordinal
)
$draftCheckoutIndex = $draftJob.IndexOf(
    "uses: actions/checkout@",
    [StringComparison]::Ordinal
)
if ($draftScriptIndex -lt 0 -or
    $draftCheckoutIndex -lt 0 -or
    $draftCheckoutIndex -gt $draftScriptIndex) {
    throw "draft job must check out the exact source before invoking its repository script"
}

$forbidden = @(
    "pull_request_target",
    "cargo publish",
    "npm publish",
    "--latest=true",
    "sha256sum --check"
)
foreach ($text in $forbidden) {
    if ($workflow.Contains($text)) { throw "release workflow contains forbidden behavior: $text" }
}
if ($workflow -match '(?m)^\s*permissions:\s*write-all') {
    throw "release workflow cannot use write-all permissions"
}
Write-Output "release workflow verified: immutable actions, exact matrix, least-privilege draft-only boundary"
