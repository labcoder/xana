$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$testParent = [IO.Path]::GetTempPath()
$testRoot = Join-Path $testParent ("xana-removal-test-" + [Guid]::NewGuid().ToString("N"))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null

try {
    $stateRoot = Join-Path $testRoot "state"
    $session = Join-Path $stateRoot "sessions\keep.jsonl"
    $artifact = Join-Path $stateRoot "artifacts\keep.bin"
    $config = Join-Path $stateRoot "config.toml"
    [IO.Directory]::CreateDirectory((Split-Path -Parent $session)) | Out-Null
    [IO.Directory]::CreateDirectory((Split-Path -Parent $artifact)) | Out-Null
    [IO.File]::WriteAllText($session, "session-preserved")
    [IO.File]::WriteAllText($artifact, "artifact-preserved")
    [IO.File]::WriteAllText($config, "config-preserved")

    $unixHome = Join-Path $testRoot "unix-home"
    $unixBin = Join-Path $unixHome ".local\bin"
    $unixBinary = Join-Path $unixBin "xana"
    $unixProfile = Join-Path $unixHome ".profile"
    [IO.Directory]::CreateDirectory($unixBin) | Out-Null
    [IO.File]::WriteAllText($unixBinary, "binary")
    [IO.File]::WriteAllLines($unixProfile, @(
        "export KEEP=1",
        "export PATH='$unixBin':`"`$PATH`" # xana-installer-path-v1",
        "export ALSO_KEEP=1"
    ))
    [IO.File]::Delete($unixBinary)
    $profileLines = @([IO.File]::ReadAllLines($unixProfile) | Where-Object {
        -not $_.EndsWith("# xana-installer-path-v1", [StringComparison]::Ordinal)
    })
    [IO.File]::WriteAllLines($unixProfile, $profileLines)
    if ((Test-Path -LiteralPath $unixBinary) -or
        ([IO.File]::ReadAllText($unixProfile)).Contains("xana-installer-path-v1") -or
        -not ([IO.File]::ReadAllText($unixProfile)).Contains("export KEEP=1")) {
        throw "Unix removal recipe removed the wrong state"
    }

    $windowsBin = Join-Path $testRoot "Programs\Xana\bin"
    $windowsBinary = Join-Path $windowsBin "xana.exe"
    [IO.Directory]::CreateDirectory($windowsBin) | Out-Null
    [IO.File]::WriteAllText($windowsBinary, "binary")
    $keepOne = "%LOCALAPPDATA%\Keep"
    $keepTwo = "C:\Tools"
    $userPath = "$keepOne;$windowsBin;$keepTwo"
    [IO.File]::Delete($windowsBinary)
    $entries = @($userPath -split ';' | Where-Object {
        -not [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\').Equals(
            $windowsBin.TrimEnd('\'),
            [StringComparison]::OrdinalIgnoreCase
        )
    })
    $updatedPath = $entries -join ';'
    if ((Test-Path -LiteralPath $windowsBinary) -or
        $updatedPath -ne "$keepOne;$keepTwo") {
        throw "Windows removal recipe changed unrelated PATH state"
    }

    foreach ($preserved in @($session, $artifact, $config)) {
        if (-not (Test-Path -LiteralPath $preserved -PathType Leaf)) {
            throw "removal recipe deleted Xana personal state"
        }
    }
    Write-Output "removal recipes verified: executable and owned PATH entry removed; config, sessions, artifacts, unrelated PATH preserved"
} finally {
    $fullRoot = [IO.Path]::GetFullPath($testRoot)
    $fullParent = [IO.Path]::GetFullPath($testParent).TrimEnd('\') + '\'
    if (-not $fullRoot.StartsWith($fullParent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($fullRoot).StartsWith("xana-removal-test-")) {
        throw "refusing unsafe removal-test cleanup"
    }
    if (Test-Path -LiteralPath $fullRoot) {
        [IO.Directory]::Delete($fullRoot, $true)
    }
}
