<#
.SYNOPSIS
Installs or updates a verified Xana Release Preview executable for Windows x64.

.DESCRIPTION
Downloads one exact four-target release manifest and its Windows x64 archive,
verifies SHA-256 and archive structure, smokes the staged executable, and then
activates it in a per-user directory. The installer never requests elevation,
does not install a language runtime, and does not use XANA_HOME for executable
placement.

.PARAMETER Version
An exact X.Y.Z release or "latest". Defaults to latest.

.PARAMETER InstallDir
The destination directory. Defaults to %LOCALAPPDATA%\Programs\Xana\bin.

.PARAMETER NoSetup
Do not invoke `xana setup --if-needed` after activation.

.PARAMETER ModifyPath
Add InstallDir to the user PATH without prompting.

.PARAMETER NoModifyPath
Never change the user PATH.
#>
[CmdletBinding()]
param(
    [ValidatePattern('^(latest|[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9})$')]
    [string]$Version = "latest",
    [string]$InstallDir,
    [switch]$NoSetup,
    [switch]$ModifyPath,
    [switch]$NoModifyPath,
    [switch]$Help,
    [switch]$AllowTestFixture,
    [string]$TestFixtureRoot,
    [string]$TestTarget,
    [string]$TestUserPathFile
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Repository = "labcoder/xana"
$ManifestName = "xana-release-manifest.txt"
$ManifestLimit = 65536L
$ArchiveLimit = 134217728L
$Target = "x86_64-pc-windows-msvc"
$ArchiveName = "xana-cli-$Target.zip"

if ($Help) {
    Write-Output @"
Install or update Xana from a verified Windows x64 Release Preview archive.

Usage: install.ps1 [-Version X.Y.Z] [-InstallDir DIR] [-NoSetup]
                   [-ModifyPath | -NoModifyPath]

The default version is latest and the default destination is
%LOCALAPPDATA%\Programs\Xana\bin. The installer never requests elevation,
uses XANA_HOME for executable placement, or installs a language runtime.
"@
    return
}
if ($ModifyPath -and $NoModifyPath) {
    throw "-ModifyPath conflicts with -NoModifyPath"
}
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw "LOCALAPPDATA is unavailable; pass an explicit -InstallDir"
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\Xana\bin"
}
if ($InstallDir.Contains(";") -or $InstallDir.Contains("`r") -or $InstallDir.Contains("`n")) {
    throw "install directory contains a character that is unsafe in Windows PATH"
}

$fixtureArguments = $AllowTestFixture.IsPresent -or
    -not [string]::IsNullOrWhiteSpace($TestFixtureRoot) -or
    -not [string]::IsNullOrWhiteSpace($TestTarget)
if ($fixtureArguments) {
    if (-not $AllowTestFixture -or
        [string]::IsNullOrWhiteSpace($TestFixtureRoot) -or
        [string]::IsNullOrWhiteSpace($TestTarget)) {
        throw "test fixtures require -AllowTestFixture, -TestFixtureRoot, and -TestTarget together"
    }
    if (-not [IO.Path]::IsPathRooted($TestFixtureRoot)) {
        throw "test fixture root must be an absolute local path"
    }
    if ($TestTarget -ne $Target) {
        throw "unsupported test target $TestTarget; the PowerShell installer supports only $Target"
    }
    $fixtureItem = Get-Item -LiteralPath $TestFixtureRoot
    if (-not $fixtureItem.PSIsContainer -or ($fixtureItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "test fixture root must be a real local directory, not a reparse point"
    }
    $TestFixtureRoot = $fixtureItem.FullName
    if (-not [string]::IsNullOrWhiteSpace($TestUserPathFile)) {
        if (-not [IO.Path]::IsPathRooted($TestUserPathFile)) {
            throw "test user PATH file must be an absolute local path"
        }
        $TestUserPathFile = [IO.Path]::GetFullPath($TestUserPathFile)
    }
    Write-Output "TEST ONLY: using local fixture authority $TestFixtureRoot"
} else {
    if (-not [string]::IsNullOrWhiteSpace($TestUserPathFile)) {
        throw "-TestUserPathFile is available only with the explicit fixture authority"
    }
    if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
            [Runtime.InteropServices.OSPlatform]::Windows)) {
        throw "the PowerShell installer supports Windows x64 only"
    }
    $osArchitecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    $processArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture
    if ($osArchitecture -ne [Runtime.InteropServices.Architecture]::X64 -or
        $processArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
        throw "unsupported or emulated Windows architecture ($osArchitecture/$processArchitecture); supported target: Windows x64 MSVC"
    }
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Protect-InstallerDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
        [Security.AccessControl.InheritanceFlags]::ObjectInherit
    $rule = [Security.AccessControl.FileSystemAccessRule]::new(
        $identity,
        [Security.AccessControl.FileSystemRights]::FullControl,
        $inheritance,
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    )
    $security.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $security
}

function Receive-BoundedFile {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][long]$Limit
    )

    if ($AllowTestFixture) {
        $sourcePath = Join-Path $TestFixtureRoot $Source
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
            throw "fixture asset is missing: $Source"
        }
        $sourceItem = Get-Item -LiteralPath $sourcePath
        if (($sourceItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            $sourceItem.Length -le 0 -or $sourceItem.Length -gt $Limit) {
            throw "fixture asset has an unsafe type or length: $Source"
        }
        [IO.File]::Copy($sourceItem.FullName, $Destination, $false)
        return
    }

    $uri = [Uri]$Source
    if ($uri.Scheme -ne [Uri]::UriSchemeHttps) {
        throw "production downloads require HTTPS"
    }

    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $true
    $handler.MaxAutomaticRedirections = 5
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromMinutes(5)
    $response = $null
    $input = $null
    $output = $null
    try {
        $response = $client.GetAsync(
            $uri,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead
        ).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            throw "download failed with HTTP status $([int]$response.StatusCode)"
        }
        if ($response.RequestMessage.RequestUri.Scheme -ne [Uri]::UriSchemeHttps) {
            throw "download redirected away from HTTPS"
        }
        $declared = $response.Content.Headers.ContentLength
        if ($null -ne $declared -and ($declared -le 0 -or $declared -gt $Limit)) {
            throw "download declared an invalid or oversized length"
        }
        $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [IO.FileStream]::new(
            $Destination,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        $buffer = [byte[]]::new(81920)
        $total = 0L
        while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $read
            if ($total -gt $Limit) {
                throw "download exceeded the maximum permitted length"
            }
            $output.Write($buffer, 0, $read)
        }
        if ($total -le 0) {
            throw "download was empty"
        }
        $output.Flush($true)
    } catch {
        if ($null -ne $output) { $output.Dispose(); $output = $null }
        if (Test-Path -LiteralPath $Destination) {
            [IO.File]::Delete($Destination)
        }
        throw
    } finally {
        if ($null -ne $output) { $output.Dispose() }
        if ($null -ne $input) { $input.Dispose() }
        if ($null -ne $response) { $response.Dispose() }
        $client.Dispose()
        $handler.Dispose()
    }
}

function Read-ReleaseManifest {
    param([Parameter(Mandatory = $true)][string]$Path)

    $releaseVersion = $null
    $artifacts = @{}
    $manifestVersionSeen = $false
    $lineNumber = 0
    foreach ($rawLine in [IO.File]::ReadAllLines($Path)) {
        $lineNumber++
        $line = $rawLine.TrimEnd("`r")
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $fields = @($line -split ' ' | Where-Object { $_ -ne "" })
        switch ($fields[0]) {
            "manifest-version" {
                if ($manifestVersionSeen -or $fields.Count -ne 2 -or $fields[1] -ne "1") {
                    throw "unsupported release manifest version"
                }
                $manifestVersionSeen = $true
            }
            "release-version" {
                if ($null -ne $releaseVersion -or $fields.Count -ne 2 -or
                    $fields[1] -notmatch '^[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9}$') {
                    throw "release manifest has an invalid release version"
                }
                $releaseVersion = $fields[1]
            }
            "artifact" {
                if ($fields.Count -ne 5) {
                    throw "release manifest line $lineNumber has an invalid artifact record"
                }
                $artifactTarget = $fields[1]
                $expectedName = switch ($artifactTarget) {
                    "aarch64-apple-darwin" { "xana-cli-$artifactTarget.tar.gz" }
                    "x86_64-apple-darwin" { "xana-cli-$artifactTarget.tar.gz" }
                    "x86_64-pc-windows-msvc" { "xana-cli-$artifactTarget.zip" }
                    "x86_64-unknown-linux-gnu" { "xana-cli-$artifactTarget.tar.gz" }
                    default { throw "release manifest contains an unsupported target" }
                }
                if ($fields[2] -cne $expectedName -or $fields[3] -cnotmatch '^[0-9a-f]{64}$') {
                    throw "release manifest contains an invalid artifact name or SHA-256"
                }
                $size = 0L
                if (-not [long]::TryParse($fields[4], [ref]$size) -or $size -le 0 -or $size -gt $ArchiveLimit) {
                    throw "release manifest contains an invalid artifact size"
                }
                if ($artifacts.ContainsKey($artifactTarget)) {
                    throw "release manifest contains a duplicate target"
                }
                $artifacts[$artifactTarget] = [pscustomobject]@{
                    Name = $fields[2]
                    Hash = $fields[3]
                    Size = $size
                }
            }
            default { throw "release manifest line $lineNumber has an unknown record" }
        }
    }
    $expectedTargets = @(
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu"
    )
    if (-not $manifestVersionSeen -or $null -eq $releaseVersion -or $artifacts.Count -ne 4) {
        throw "release manifest does not contain the exact release inventory"
    }
    foreach ($expectedTarget in $expectedTargets) {
        if (-not $artifacts.ContainsKey($expectedTarget)) {
            throw "release manifest is missing target $expectedTarget"
        }
    }
    [pscustomobject]@{ Version = $releaseVersion; Artifacts = $artifacts }
}

function Test-InteractiveTerminal {
    try {
        [Environment]::UserInteractive -and
            -not [Console]::IsInputRedirected -and
            -not [Console]::IsOutputRedirected
    } catch {
        $false
    }
}

function Test-PathContains {
    param([string]$PathValue, [string]$Directory)
    if ([string]::IsNullOrWhiteSpace($PathValue)) { return $false }
    $normalized = $Directory.TrimEnd('\')
    foreach ($entry in $PathValue.Split(';')) {
        if ([string]::IsNullOrWhiteSpace($entry)) { continue }
        $expanded = [Environment]::ExpandEnvironmentVariables($entry.Trim()).TrimEnd('\')
        if ($expanded.Equals($normalized, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    $false
}

function Get-UserPathValue {
    if (-not [string]::IsNullOrWhiteSpace($TestUserPathFile)) {
        if (Test-Path -LiteralPath $TestUserPathFile) {
            return [IO.File]::ReadAllText($TestUserPathFile)
        }
        return $null
    }
    [Environment]::GetEnvironmentVariable("Path", "User")
}

function Set-UserPathValue {
    param([Parameter(Mandatory = $true)][string]$Value)
    if (-not [string]::IsNullOrWhiteSpace($TestUserPathFile)) {
        [IO.File]::WriteAllText($TestUserPathFile, $Value, [Text.UTF8Encoding]::new($false))
        return
    }
    [Environment]::SetEnvironmentVariable("Path", $Value, "User")
}

function Update-UserPath {
    param([Parameter(Mandatory = $true)][string]$Directory)

    if (Test-PathContains $env:PATH $Directory) {
        Write-Output "PATH: $Directory is already active."
        return
    }
    if ($NoModifyPath) {
        Write-Output "PATH unchanged. Add $Directory to your user PATH, then open a new terminal."
        return
    }
    $shouldModify = $ModifyPath.IsPresent
    if (-not $ModifyPath -and (Test-InteractiveTerminal)) {
        $answer = Read-Host "Add $Directory to your user PATH? [y/N]"
        $shouldModify = $answer -match '^(?i:y|yes)$'
    }
    if (-not $shouldModify) {
        Write-Output "PATH unchanged. Add $Directory to your user PATH, then open a new terminal."
        return
    }

    $userPath = Get-UserPathValue
    if (Test-PathContains $userPath $Directory) {
        Write-Output "PATH: the user entry already contains $Directory; open a new terminal."
        return
    }
    $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
        $Directory
    } else {
        "$($userPath.TrimEnd(';'));$Directory"
    }
    Set-UserPathValue -Value $newPath
    Write-Output "PATH: added $Directory to the user PATH; open a new terminal."
}

function Remove-SafeDirectory {
    param([string]$Path, [string]$RequiredParent, [string]$RequiredPrefix)
    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path -PathType Container)) {
        return
    }
    $fullPath = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetFullPath($RequiredParent).TrimEnd('\') + '\'
    if (-not $fullPath.StartsWith($parent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($fullPath).StartsWith($RequiredPrefix, [StringComparison]::Ordinal)) {
        throw "refusing to clean an unexpected installer directory"
    }
    [IO.Directory]::Delete($fullPath, $true)
}

$temporaryRoot = $null
$stageDirectory = $null
$backupPath = $null
$finalPath = $null
$createdInstallDirectory = $false
$activationStarted = $false
$activationCommitted = $false
$hadPrevious = $false

try {
    $temporaryParent = [IO.Path]::GetTempPath()
    $temporaryRoot = Join-Path $temporaryParent ("xana-install-" + [Guid]::NewGuid().ToString("N"))
    [IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
    Protect-InstallerDirectory -Path $temporaryRoot
    $manifestPath = Join-Path $temporaryRoot $ManifestName
    $archivePath = Join-Path $temporaryRoot $ArchiveName

    if ($AllowTestFixture) {
        $manifestSource = $ManifestName
    } elseif ($Version -eq "latest") {
        $manifestSource = "https://github.com/$Repository/releases/latest/download/$ManifestName"
    } else {
        $manifestSource = "https://github.com/$Repository/releases/download/v$Version/$ManifestName"
    }
    Receive-BoundedFile -Source $manifestSource -Destination $manifestPath -Limit $ManifestLimit
    $manifest = Read-ReleaseManifest -Path $manifestPath
    if ($Version -ne "latest" -and $manifest.Version -ne $Version) {
        throw "requested $Version, but the manifest describes $($manifest.Version)"
    }
    $selected = $manifest.Artifacts[$Target]

    $archiveSource = if ($AllowTestFixture) {
        $ArchiveName
    } else {
        "https://github.com/$Repository/releases/download/v$($manifest.Version)/$ArchiveName"
    }
    Receive-BoundedFile -Source $archiveSource -Destination $archivePath -Limit $ArchiveLimit
    $archiveItem = Get-Item -LiteralPath $archivePath
    if ($archiveItem.Length -ne $selected.Size) {
        throw "archive length differs from the release manifest"
    }
    if ((Get-Sha256 $archivePath) -cne $selected.Hash) {
        throw "archive SHA-256 differs from the release manifest"
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $expectedEntries = @("LICENSE", "README.md", "installation.md", "xana.exe")
        if ($zip.Entries.Count -ne $expectedEntries.Count) {
            throw "Xana archive does not contain the exact four-file inventory"
        }
        $seenEntries = @{}
        $expandedLength = 0L
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            if ([string]::IsNullOrEmpty($entry.Name) -or $name.StartsWith('/') -or
                $name.Contains(':') -or $name -match '(^|/)\.\.(/|$)') {
                throw "Xana archive contains an unsafe path"
            }
            if ($expectedEntries -cnotcontains $name -or $seenEntries.ContainsKey($name)) {
                throw "Xana archive contents differ from the release contract"
            }
            $unixType = (($entry.ExternalAttributes -shr 16) -band 0xF000)
            if (($unixType -ne 0 -and $unixType -ne 0x8000) -or
                ($entry.ExternalAttributes -band [int][IO.FileAttributes]::ReparsePoint)) {
                throw "Xana archive contains a link or reparse entry"
            }
            if ($entry.Length -le 0 -or $entry.Length -gt $ArchiveLimit) {
                throw "Xana archive contains an invalid or oversized entry"
            }
            $expandedLength += $entry.Length
            if ($expandedLength -gt ($ArchiveLimit * 2)) {
                throw "Xana archive expands beyond the permitted bound"
            }
            $seenEntries[$name] = $entry
        }
        if ($seenEntries.Count -ne 4) {
            throw "Xana archive does not contain the exact four-file inventory"
        }

        if (-not [IO.Path]::IsPathRooted($InstallDir)) {
            throw "install directory must be an absolute Windows path"
        }
        $InstallDir = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
        if (-not (Test-Path -LiteralPath $InstallDir)) {
            [IO.Directory]::CreateDirectory($InstallDir) | Out-Null
            $createdInstallDirectory = $true
        }
        $installItem = Get-Item -LiteralPath $InstallDir
        if (-not $installItem.PSIsContainer -or ($installItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "install directory must be a real directory, not a reparse point"
        }
        $InstallDir = $installItem.FullName.TrimEnd('\')
        $stageDirectory = Join-Path $InstallDir (".xana-stage-" + [Guid]::NewGuid().ToString("N"))
        [IO.Directory]::CreateDirectory($stageDirectory) | Out-Null
        Protect-InstallerDirectory -Path $stageDirectory
        $stagedExecutable = Join-Path $stageDirectory "xana.exe"
        $entryStream = $seenEntries["xana.exe"].Open()
        $stagedStream = [IO.FileStream]::new(
            $stagedExecutable,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        try {
            $buffer = [byte[]]::new(81920)
            $written = 0L
            while (($read = $entryStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $written += $read
                if ($written -gt $ArchiveLimit) {
                    throw "Xana executable expands beyond the permitted bound"
                }
                $stagedStream.Write($buffer, 0, $read)
            }
            if ($written -ne $seenEntries["xana.exe"].Length) {
                throw "Xana executable length differs from its ZIP metadata"
            }
            $stagedStream.Flush($true)
        } finally {
            $stagedStream.Dispose()
            $entryStream.Dispose()
        }
    } finally {
        $zip.Dispose()
    }

    $stagedVersionOutput = (& $stagedExecutable --version 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $stagedVersionOutput -cne "xana $($manifest.Version)") {
        throw "staged Xana version smoke failed"
    }
    & $stagedExecutable --help *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "staged Xana help smoke failed"
    }

    $finalPath = Join-Path $InstallDir "xana.exe"
    $existingVersion = $null
    $action = "install"
    if (Test-Path -LiteralPath $finalPath) {
        $existingItem = Get-Item -LiteralPath $finalPath
        if ($existingItem.PSIsContainer -or ($existingItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "refusing to replace a directory or reparse-point destination"
        }
        $hadPrevious = $true
        $existingOutput = (& $finalPath --version 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -eq 0 -and $existingOutput -match '^xana ([0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9})$') {
            $existingVersion = $Matches[1]
        }
        if ($existingVersion -eq $manifest.Version) {
            $action = "reinstall"
        } elseif ($null -eq $existingVersion) {
            $action = "replace-unrecognized"
        } elseif ([version]$manifest.Version -gt [version]$existingVersion) {
            $action = "upgrade"
        } else {
            $action = "downgrade"
        }
    }

    $activationStarted = $true
    if ($hadPrevious) {
        $backupPath = Join-Path $InstallDir (".xana-backup-" + [Guid]::NewGuid().ToString("N"))
        [IO.File]::Replace($stagedExecutable, $finalPath, $backupPath, $true)
    } else {
        [IO.File]::Move($stagedExecutable, $finalPath)
    }
    try {
        Update-UserPath -Directory $InstallDir
    } catch {
        if ($hadPrevious -and (Test-Path -LiteralPath $backupPath -PathType Leaf)) {
            [IO.File]::Replace($backupPath, $finalPath, $null, $true)
            $backupPath = $null
        } elseif (-not $hadPrevious -and (Test-Path -LiteralPath $finalPath -PathType Leaf)) {
            [IO.File]::Delete($finalPath)
        }
        throw "PATH update failed; executable activation was rolled back: $($_.Exception.Message)"
    }
    $activationCommitted = $true
    if ($null -ne $backupPath -and (Test-Path -LiteralPath $backupPath -PathType Leaf)) {
        [IO.File]::Delete($backupPath)
        $backupPath = $null
    }
    $createdInstallDirectory = $false

    Write-Output "Xana $action`: $($manifest.Version) $Target at $finalPath (SHA-256 verified)."
    $setupResult = "skipped"
    if (-not $NoSetup) {
        & $finalPath setup --if-needed
        $setupStatus = $LASTEXITCODE
        switch ($setupStatus) {
            0 { $setupResult = "ready-or-configured" }
            10 {
                $setupResult = "pending"
                Write-Output "Xana configuration is pending. Run: xana setup"
            }
            default {
                throw "Xana binary activation succeeded, but setup readiness failed; run '$finalPath doctor'"
            }
        }
    }
    Write-Output "Install receipt: version=$($manifest.Version) action=$action target=$Target setup=$setupResult"
} catch {
    $originalError = $_.Exception.Message
    $restoreError = $null
    if ($activationStarted -and -not $activationCommitted -and $null -ne $finalPath) {
        try {
            if ($hadPrevious -and $null -ne $backupPath -and (Test-Path -LiteralPath $backupPath -PathType Leaf)) {
                [IO.File]::Replace($backupPath, $finalPath, $null, $true)
                $backupPath = $null
            } elseif (-not $hadPrevious -and (Test-Path -LiteralPath $finalPath -PathType Leaf)) {
                [IO.File]::Delete($finalPath)
            }
        } catch {
            $restoreError = $_.Exception.Message
        }
    }
    if ($null -ne $restoreError) {
        throw "Xana installer error: $originalError; the prior executable could not be restored: $restoreError"
    }
    throw "Xana installer error: $originalError"
} finally {
    if ($null -ne $stageDirectory) {
        Remove-SafeDirectory -Path $stageDirectory -RequiredParent $InstallDir -RequiredPrefix ".xana-stage-"
    }
    if ($null -ne $temporaryRoot) {
        Remove-SafeDirectory -Path $temporaryRoot -RequiredParent ([IO.Path]::GetTempPath()) -RequiredPrefix "xana-install-"
    }
    if ($createdInstallDirectory -and (Test-Path -LiteralPath $InstallDir -PathType Container)) {
        $remaining = @(Get-ChildItem -Force -LiteralPath $InstallDir)
        if ($remaining.Count -eq 0) {
            [IO.Directory]::Delete($InstallDir, $false)
        }
    }
}
