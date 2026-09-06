$ErrorActionPreference = "Stop"

# Use Cargo's real inventory once; inject private paths without creating files.
$cargoExecutable = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
$packageFixtureBase = @(& $cargoExecutable package --list --locked --allow-dirty)
if ($LASTEXITCODE -ne 0) { throw "could not collect package audit fixture" }
$packageFixtureBase = @($packageFixtureBase | Where-Object { $_ -cne ".cargo/config.toml" })
$packageAuditScript = Join-Path $PSScriptRoot "check-package-contents.ps1"

function Invoke-PackageFixture {
    param([string[]]$Entries)
    # Scoped shadow: the audit exercises its normal Cargo-output boundary.
    function cargo {
        $global:LASTEXITCODE = 0
        $Entries
    }
    & $packageAuditScript -AllowDirty | Out-Null
}

Invoke-PackageFixture -Entries ($packageFixtureBase + ".cargo/config.toml")

$privatePaths = @(
    "config.toml",
    "private/config.toml",
    "private/.cargo/config.toml",
    ".CARGO/config.toml",
    ".cargo/CONFIG.toml",
    "target/private.txt",
    ".xana/private.txt",
    "course/private.txt",
    "book/private.txt",
    ".env",
    "private/.env.local",
    "sessions/private.txt",
    "artifacts/private.txt"
)
foreach ($privatePath in $privatePaths) {
    $rejected = $false
    try {
        Invoke-PackageFixture -Entries ($packageFixtureBase + ".cargo/config.toml" + $privatePath)
    } catch {
        if ($_.Exception.Message -notlike "forbidden package path matched *: $privatePath") { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw "package audit accepted private fixture: $privatePath" }
}

Write-Output "package audit verified: exact Cargo build config allowed; $($privatePaths.Count) private paths rejected"
