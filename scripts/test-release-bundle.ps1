$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$version = "0.5.0"
$tag = "v$version"
$testParent = [IO.Path]::GetTempPath()
$testRoot = Join-Path $testParent ("xana-release-bundle-test-" + [Guid]::NewGuid().ToString("N"))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null

$archives = @(
    "xana-aarch64-apple-darwin.tar.gz",
    "xana-x86_64-apple-darwin.tar.gz",
    "xana-x86_64-pc-windows-msvc.zip",
    "xana-x86_64-unknown-linux-gnu.tar.gz"
)

function Reset-Fixture {
    Get-ChildItem -File -LiteralPath $testRoot | Remove-Item -Force
    foreach ($archive in $archives) {
        $archivePath = Join-Path $testRoot $archive
        [IO.File]::WriteAllText($archivePath, "fixture bytes for $archive")
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
        [IO.File]::WriteAllText("$archivePath.sha256", "$hash  $archive`n")
    }
    $distManifest = [ordered]@{
        dist_version = "0.32.0"
        announcement_tag = $tag
        releases = @([ordered]@{ app_name = "xana"; app_version = $version })
    } | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText(
        (Join-Path $testRoot "dist-manifest.json"),
        $distManifest,
        [Text.UTF8Encoding]::new($false)
    )
}

try {
    Reset-Fixture
    & "$PSScriptRoot/assemble-release-bundle.ps1" `
        -ArtifactDirectory $testRoot `
        -Version $version `
        -Tag $tag *> $null
    $files = @(Get-ChildItem -File -LiteralPath $testRoot)
    if ($files.Count -ne 15 -or
        -not (Test-Path -LiteralPath (Join-Path $testRoot "sha256.sum")) -or
        -not (Test-Path -LiteralPath (Join-Path $testRoot "xana-installer.sh")) -or
        -not (Test-Path -LiteralPath (Join-Path $testRoot "xana-installer.ps1"))) {
        throw "assembled fixture does not contain the exact public bundle"
    }

    Reset-Fixture
    [IO.File]::WriteAllText((Join-Path $testRoot "unexpected"), "unexpected")
    $rejectedUnexpected = $false
    try {
        & "$PSScriptRoot/assemble-release-bundle.ps1" `
            -ArtifactDirectory $testRoot -Version $version -Tag $tag *> $null
    } catch { $rejectedUnexpected = $true }
    if (-not $rejectedUnexpected) { throw "unexpected assembly input was accepted" }

    Reset-Fixture
    [IO.File]::WriteAllText(
        (Join-Path $testRoot "xana-x86_64-pc-windows-msvc.zip.sha256"),
        ("0" * 64) + "  xana-x86_64-pc-windows-msvc.zip`n"
    )
    $rejectedHash = $false
    try {
        & "$PSScriptRoot/assemble-release-bundle.ps1" `
            -ArtifactDirectory $testRoot -Version $version -Tag $tag *> $null
    } catch { $rejectedHash = $true }
    if (-not $rejectedHash) { throw "mismatched native checksum was accepted" }

    Write-Output "release bundle verified: exact inventory, source wrappers, checksums, mismatch rejection"
} finally {
    $fullRoot = [IO.Path]::GetFullPath($testRoot)
    $fullParent = [IO.Path]::GetFullPath($testParent).TrimEnd('\') + '\'
    if (-not $fullRoot.StartsWith($fullParent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($fullRoot).StartsWith("xana-release-bundle-test-")) {
        throw "refusing unsafe release-bundle test cleanup"
    }
    if (Test-Path -LiteralPath $fullRoot) {
        [IO.Directory]::Delete($fullRoot, $true)
    }
}
