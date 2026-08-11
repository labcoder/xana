[CmdletBinding()]
param(
    [string]$DistProgram = "dist",
    [string]$Tag
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$expectedDistVersion = "0.32.0"
$expectedApp = "xana"
$expectedTargets = @(
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu"
)

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-SameSet {
    param([string[]]$Actual, [string[]]$Expected, [string]$Label)
    $difference = @(Compare-Object ($Actual | Sort-Object) ($Expected | Sort-Object))
    if ($difference.Count -ne 0) {
        $actualText = ($Actual | Sort-Object) -join ", "
        $expectedText = ($Expected | Sort-Object) -join ", "
        throw "$Label mismatch. Expected [$expectedText], got [$actualText]"
    }
}

$workspaceManifest = Get-Content -Raw -LiteralPath "Cargo.toml"
$distConfig = Get-Content -Raw -LiteralPath "dist-workspace.toml"
Assert-True ($distConfig -match '(?m)^allow-dirty\s*=\s*\["ci"\]\s*$') (
    "cargo-dist must leave Xana's reviewed release workflow under repository control"
)
$versionMatch = [regex]::Match(
    $workspaceManifest,
    '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(?<version>[^"]+)"'
)
Assert-True $versionMatch.Success "could not read workspace version"
$workspaceVersion = $versionMatch.Groups["version"].Value

if ([string]::IsNullOrWhiteSpace($Tag)) {
    $Tag = "v$workspaceVersion"
}
Assert-True ($Tag -eq "v$workspaceVersion") "tag $Tag does not match workspace version $workspaceVersion"

$versionOutput = (& $DistProgram --version 2>&1 | Out-String).Trim()
Assert-True ($LASTEXITCODE -eq 0) "could not execute pinned dist planner"
Assert-True ($versionOutput -eq "cargo-dist $expectedDistVersion") (
    "expected cargo-dist $expectedDistVersion, got '$versionOutput'"
)

$planText = (& $DistProgram plan --tag $Tag --allow-dirty --output-format json --no-local-paths 2>&1 | Out-String)
Assert-True ($LASTEXITCODE -eq 0) "dist plan failed: $planText"
$plan = $planText | ConvertFrom-Json

Assert-True ($plan.dist_version -eq $expectedDistVersion) "plan used an unexpected dist version"
Assert-True ($plan.announcement_tag -eq $Tag) "plan tag differs from the requested tag"
Assert-True (@($plan.releases).Count -eq 1) "release plan must contain exactly one application"
Assert-True ($plan.releases[0].app_name -eq $expectedApp) "release plan selected an unexpected application"
Assert-True ($plan.releases[0].app_version -eq $workspaceVersion) "release plan version differs from Cargo"
Assert-True ($plan.releases[0].display_name -eq "Xana") "release display name must be Xana"
Assert-True ([bool]$plan.github_attestations) "GitHub attestation intent must remain enabled"

$expectedArchives = @($expectedTargets | ForEach-Object {
    if ($_ -eq "x86_64-pc-windows-msvc") {
        "$expectedApp-$_.zip"
    } else {
        "$expectedApp-$_.tar.gz"
    }
})
$expectedArtifacts = @("sha256.sum")
foreach ($archive in $expectedArchives) {
    $expectedArtifacts += $archive
    $expectedArtifacts += "$archive.sha256"
}
$actualArtifacts = @($plan.artifacts.PSObject.Properties.Name)
Assert-SameSet $actualArtifacts $expectedArtifacts "release artifact inventory"

$actualTargets = @()
foreach ($archive in $expectedArchives) {
    $artifact = $plan.artifacts.$archive
    Assert-True ($null -ne $artifact) "missing planned archive $archive"
    Assert-True (@($artifact.target_triples).Count -eq 1) "$archive must name exactly one target"
    $actualTargets += [string]$artifact.target_triples[0]
    Assert-True ($artifact.checksum -eq "$archive.sha256") "$archive must use its SHA-256 sidecar"

    $assetNames = @($artifact.assets | ForEach-Object { [string]$_.name })
    Assert-SameSet $assetNames @("LICENSE", "README.md", "installation.md", "xana") "$archive contents"
}
Assert-SameSet $actualTargets $expectedTargets "release target matrix"

$ciTargets = @($plan.ci.github.artifacts_matrix.include | ForEach-Object { [string]$_.host })
Assert-SameSet $ciTargets $expectedTargets "native runner matrix"

Assert-True (-not $planText.Contains((Get-Location).Path)) "release plan leaked a local workspace path"
Write-Output "release plan verified: Xana $workspaceVersion, four native targets, SHA-256, attestations"
