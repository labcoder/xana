$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. "$PSScriptRoot/fixture-cleanup.ps1"
Assert-FixtureCleanupGuardContract

$commit = "0123456789abcdef0123456789abcdef01234567"
$version = "0.5.1"
$tag = "v$version"
$testParent = [IO.Path]::GetTempPath()
$testRoot = Join-Path $testParent ("xana-release-draft-test-" + [Guid]::NewGuid().ToString("N"))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null

$assetNames = @(
    "dist-manifest.json",
    "sha256.sum",
    "xana-$version-release-notes.md",
    "xana-aarch64-apple-darwin.tar.gz",
    "xana-aarch64-apple-darwin.tar.gz.sha256",
    "xana-installer.ps1",
    "xana-installer.sh",
    "xana-release-manifest.txt",
    "xana-release-review-checklist.md",
    "xana-x86_64-apple-darwin.tar.gz",
    "xana-x86_64-apple-darwin.tar.gz.sha256",
    "xana-x86_64-pc-windows-msvc.zip",
    "xana-x86_64-pc-windows-msvc.zip.sha256",
    "xana-x86_64-unknown-linux-gnu.tar.gz",
    "xana-x86_64-unknown-linux-gnu.tar.gz.sha256"
)
foreach ($name in $assetNames) {
    [IO.File]::WriteAllText((Join-Path $testRoot $name), "fixture $name`n")
}

$savedEnvironment = @{}
foreach ($name in @(
    "GITHUB_EVENT_NAME",
    "GITHUB_REF",
    "RELEASE_TAG",
    "RELEASE_VERSION",
    "GITHUB_REPOSITORY",
    "GITHUB_SHA"
)) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:GITHUB_EVENT_NAME = "push"
$env:GITHUB_REF = "refs/tags/$tag"
$env:RELEASE_TAG = $tag
$env:RELEASE_VERSION = $version
$env:GITHUB_REPOSITORY = "labcoder/xana"
$env:GITHUB_SHA = $commit

$global:XanaDraftTestCommit = $commit
$global:XanaDraftTestTag = $tag
$global:XanaDraftTestExists = $false
$global:XanaDraftTestDraft = $true
$global:XanaDraftTestTitle = ""
$global:XanaDraftTestUploads = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$global:XanaDraftTestCommands = [Collections.Generic.List[string]]::new()
$global:XanaDraftTestViewCount = 0

function gh {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)

    $global:XanaDraftTestCommands.Add(($Arguments -join " "))
    if ($Arguments[0] -eq "api") {
        if ($Arguments[1] -eq "repos/labcoder/xana/git/ref/tags/$global:XanaDraftTestTag") {
            $global:LASTEXITCODE = 0
            return (@{ object = @{ type = "commit"; sha = $global:XanaDraftTestCommit } } | ConvertTo-Json -Compress)
        }
        if ($Arguments[1] -eq "repos/labcoder/xana/releases/tags/$global:XanaDraftTestTag") {
            [Console]::Error.WriteLine("gh: Not Found (HTTP 404)")
            $global:LASTEXITCODE = 1
            return
        }
    }
    if ($Arguments[0] -eq "release" -and $Arguments[1] -eq "view") {
        $global:XanaDraftTestViewCount++
        if (-not $global:XanaDraftTestExists) {
            $global:LASTEXITCODE = 1
            return
        }
        $global:LASTEXITCODE = 0
        return (@{
            tagName = $global:XanaDraftTestTag
            targetCommitish = $global:XanaDraftTestCommit
            isDraft = $global:XanaDraftTestDraft
            name = $global:XanaDraftTestTitle
            assets = @($global:XanaDraftTestUploads | Sort-Object | ForEach-Object { @{ name = $_ } })
        } | ConvertTo-Json -Depth 4 -Compress)
    }
    if ($Arguments[0] -eq "release" -and $Arguments[1] -eq "create") {
        if ($global:XanaDraftTestExists) {
            [Console]::Error.WriteLine("release already exists")
            $global:LASTEXITCODE = 1
            return
        }
        $global:XanaDraftTestExists = $true
        $titleIndex = [Array]::IndexOf($Arguments, "--title")
        $global:XanaDraftTestTitle = $Arguments[$titleIndex + 1]
        $global:LASTEXITCODE = 0
        return "https://github.com/labcoder/xana/releases/tag/untagged-fixture"
    }
    if ($Arguments[0] -eq "release" -and $Arguments[1] -eq "edit") {
        $titleIndex = [Array]::IndexOf($Arguments, "--title")
        $global:XanaDraftTestTitle = $Arguments[$titleIndex + 1]
        $global:LASTEXITCODE = 0
        return
    }
    if ($Arguments[0] -eq "release" -and $Arguments[1] -eq "upload") {
        [void]$global:XanaDraftTestUploads.Add([IO.Path]::GetFileName($Arguments[3]))
        $global:LASTEXITCODE = 0
        return
    }
    throw "unexpected fake gh command: $($Arguments -join ' ')"
}

try {
    & "$PSScriptRoot/create-release-draft.ps1" -ArtifactDirectory $testRoot *> $null
    if ($global:XanaDraftTestUploads.Count -ne 15) { throw "draft helper did not upload the exact asset inventory" }
    if ($global:XanaDraftTestTitle -ne "Xana $version Developer Preview") {
        throw "verified draft did not receive its clean public title"
    }
    if ($global:XanaDraftTestViewCount -ne 2) { throw "draft helper did not resolve before and after upload" }
    if (@($global:XanaDraftTestCommands | Where-Object { $_ -match '/releases/tags/' }).Count -ne 0) {
        throw "draft helper used the draft-blind release-by-tag REST endpoint"
    }

    $global:XanaDraftTestViewCount = 0
    & "$PSScriptRoot/create-release-draft.ps1" -ArtifactDirectory $testRoot *> $null
    if ($global:XanaDraftTestViewCount -ne 2 -or $global:XanaDraftTestUploads.Count -ne 15 -or
        $global:XanaDraftTestTitle -ne "Xana $version Developer Preview") {
        throw "draft reconciliation is not idempotent"
    }

    $global:XanaDraftTestDraft = $false
    $publishedRejected = $false
    try {
        & "$PSScriptRoot/create-release-draft.ps1" -ArtifactDirectory $testRoot *> $null
    } catch {
        $publishedRejected = $_.Exception.Message -eq "refusing to modify an already published release"
    }
    if (-not $publishedRejected) { throw "published release was not rejected" }

    Write-Output "release draft verified: draft-aware lookup, exact inventory, clean title, idempotency, published refusal"
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name])
    }
    Remove-Item Function:\gh -ErrorAction SilentlyContinue
    Remove-Variable -Scope Global -Name XanaDraftTestCommit, XanaDraftTestTag, `
        XanaDraftTestExists, XanaDraftTestDraft, XanaDraftTestTitle, `
        XanaDraftTestUploads, XanaDraftTestCommands, XanaDraftTestViewCount `
        -ErrorAction SilentlyContinue
    $fullRoot = [IO.Path]::GetFullPath($testRoot)
    $fullParent = [IO.Path]::GetFullPath($testParent)
    if (-not (Test-SafeFixtureCleanupRoot `
            -Parent $fullParent `
            -Root $fullRoot `
            -Separator ([IO.Path]::DirectorySeparatorChar) `
            -RequiredPrefix "xana-release-draft-test-")) {
        throw "refusing unsafe release-draft test cleanup"
    }
    if (Test-Path -LiteralPath $fullRoot) {
        [IO.Directory]::Delete($fullRoot, $true)
    }
}
