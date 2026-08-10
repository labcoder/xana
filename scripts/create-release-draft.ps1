[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ArtifactDirectory)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($env:GITHUB_EVENT_NAME -ne "push" -or $env:GITHUB_REF -ne "refs/tags/$($env:RELEASE_TAG)") {
    throw "draft creation is permitted only for the exact pushed release tag"
}
if ($env:RELEASE_TAG -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$' -or
    $env:RELEASE_TAG -ne "v$($env:RELEASE_VERSION)") {
    throw "release tag and version are inconsistent"
}

$root = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$assets = @(Get-ChildItem -File -LiteralPath $root | Sort-Object Name)
if ($assets.Count -ne 15) { throw "draft requires the complete fifteen-asset bundle" }
$expectedNames = @($assets | ForEach-Object Name)
$notesPath = Join-Path $root "xana-$($env:RELEASE_VERSION)-release-notes.md"

$tagRef = (& gh api "repos/$($env:GITHUB_REPOSITORY)/git/ref/tags/$($env:RELEASE_TAG)" | Out-String) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "could not resolve the pushed release tag" }
$tagObject = $tagRef.object
$tagDepth = 0
while ($tagObject.type -eq "tag" -and $tagDepth -lt 4) {
    $tagObject = ((& gh api "repos/$($env:GITHUB_REPOSITORY)/git/tags/$($tagObject.sha)" | Out-String) | ConvertFrom-Json).object
    if ($LASTEXITCODE -ne 0) { throw "could not resolve annotated release tag" }
    $tagDepth++
}
if ($tagObject.type -ne "commit" -or $tagObject.sha -ne $env:GITHUB_SHA) {
    throw "release tag does not resolve to the workflow commit"
}

$existingJson = (& gh api "repos/$($env:GITHUB_REPOSITORY)/releases/tags/$($env:RELEASE_TAG)" 2>$null | Out-String)
$exists = $LASTEXITCODE -eq 0
if ($exists) {
    $release = $existingJson | ConvertFrom-Json
    if (-not $release.draft) { throw "refusing to modify an already published release" }
    $unexpected = @($release.assets | ForEach-Object name | Where-Object { $expectedNames -notcontains $_ })
    if ($unexpected.Count -ne 0) {
        throw "existing draft contains unexpected assets: $($unexpected -join ', ')"
    }
    & gh release edit $env:RELEASE_TAG `
        --draft `
        --title "INCOMPLETE - Xana $($env:RELEASE_VERSION) Developer Preview" `
        --notes-file $notesPath
    if ($LASTEXITCODE -ne 0) { throw "could not mark the existing draft incomplete" }
} else {
    & gh release create $env:RELEASE_TAG `
        --draft `
        --verify-tag `
        --target $env:GITHUB_SHA `
        --title "INCOMPLETE - Xana $($env:RELEASE_VERSION) Developer Preview" `
        --notes-file $notesPath
    if ($LASTEXITCODE -ne 0) { throw "could not create the GitHub draft" }
}

foreach ($asset in $assets) {
    & gh release upload $env:RELEASE_TAG $asset.FullName --clobber
    if ($LASTEXITCODE -ne 0) { throw "could not upload $($asset.Name); draft remains incomplete" }
}

$remoteJson = (& gh api "repos/$($env:GITHUB_REPOSITORY)/releases/tags/$($env:RELEASE_TAG)" | Out-String)
if ($LASTEXITCODE -ne 0) { throw "could not verify the assembled draft" }
$remote = $remoteJson | ConvertFrom-Json
$remoteNames = @($remote.assets | ForEach-Object name | Sort-Object)
$difference = @(Compare-Object ($expectedNames | Sort-Object) $remoteNames)
if ($difference.Count -ne 0 -or -not $remote.draft) {
    throw "GitHub draft inventory or attribution differs from the local bundle"
}

& gh release edit $env:RELEASE_TAG `
    --draft `
    --title "REVIEW READY - Xana $($env:RELEASE_VERSION) Developer Preview" `
    --notes-file $notesPath
if ($LASTEXITCODE -ne 0) { throw "draft is complete but could not be marked review-ready" }
Write-Output "complete unpublished draft is ready for the owner checklist; publication remains a separate human action"
