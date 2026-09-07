$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)) {
    throw 'installer sharing-lock tests require native Windows'
}

# Load only the reviewed function; never run the installer's network/activation entry.
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot '../install/install.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'installer parse error' }
$definition = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Replace-InstallerExecutable'
}, $false)
if ($null -eq $definition) { throw 'installer replacement function not found' }
. ([scriptblock]::Create($definition.Extent.Text))

if ($null -eq ('XanaInstallerLockFixture' -as [type])) {
    Add-Type -TypeDefinition @'
using System.IO;
using System.Threading.Tasks;
public static class XanaInstallerLockFixture {
    public static async Task Hold(FileStream stream) {
        using (stream) { await Task.Delay(500); }
    }
}
'@
}
$parent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
$root = Join-Path $parent ('xana-replacement-test-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($root) | Out-Null
$source = Join-Path $root 'new'
$destination = Join-Path $root 'current'
$backup = Join-Path $root 'backup'
$release = $null
try {
    foreach ($lockedPath in @($source, $destination)) {
        [IO.File]::WriteAllText($source, 'new fixture')
        [IO.File]::WriteAllText($destination, 'prior fixture')
        $lock = [IO.File]::Open($lockedPath, 'Open', 'Read', 'Read')
        $release = [XanaInstallerLockFixture]::Hold($lock)
        Replace-InstallerExecutable $source $destination $backup
        [void]$release.GetAwaiter().GetResult()
        if ([IO.File]::ReadAllText($destination) -ne 'new fixture' -or
            [IO.File]::ReadAllText($backup) -ne 'prior fixture') {
            throw 'transient-lock replacement lost the new executable or exact backup'
        }
        [IO.File]::Delete($backup)
    }

    [IO.File]::WriteAllText($source, 'retry fixture')
    $lock = [IO.File]::Open($destination, 'Open', 'Read', 'Read')
    $rejected = $false
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try { Replace-InstallerExecutable $source $destination $backup }
    catch { $rejected = $true }
    finally { $lock.Dispose() }
    if (-not $rejected -or $timer.Elapsed.TotalSeconds -gt 10 -or
        [IO.File]::ReadAllText($source) -ne 'retry fixture' -or
        [IO.File]::ReadAllText($destination) -ne 'new fixture' -or
        [IO.File]::Exists($backup)) {
        throw 'persistent lock must fail boundedly without activation or losing the staged file'
    }
    # Rollback uses the same operation without creating another backup.
    Replace-InstallerExecutable $source $destination $null
    if ([IO.File]::ReadAllText($destination) -ne 'retry fixture') { throw 'no-backup replacement failed' }
    Write-Output 'installer replacement verified: transient source/destination locks, bounded permanent refusal, exact backup, rollback'
}
finally {
    if ($null -ne $release) { [void]$release.GetAwaiter().GetResult() }
    $resolved = [IO.Path]::GetFullPath($root)
    if (-not $resolved.StartsWith($parent, [StringComparison]::OrdinalIgnoreCase) -or
        -not [IO.Path]::GetFileName($resolved).StartsWith('xana-replacement-test-')) {
        throw 'refusing unsafe replacement-fixture cleanup'
    }
    [IO.Directory]::Delete($resolved, $true)
}
