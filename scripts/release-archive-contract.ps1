$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-ReleaseArchiveLayout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Entries,
        [Parameter(Mandatory = $true)]
        [ValidateSet(
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu"
        )]
        [string]$Target
    )

    $normalizedEntries = @($Entries | ForEach-Object { $_.Replace("\", "/") })
    foreach ($entry in $normalizedEntries) {
        if ([string]::IsNullOrWhiteSpace($entry) -or
            $entry.StartsWith("/", [StringComparison]::Ordinal) -or
            $entry -match '(^|/)\.\.(/|$)' -or
            $entry.Contains(":")) {
            throw "unsafe archive entry: $entry"
        }
    }

    $isWindows = $Target -eq "x86_64-pc-windows-msvc"
    $executable = if ($isWindows) { "xana.exe" } else { "xana" }
    $expectedEntries = @("LICENSE", "README.md", "installation.md", $executable) | Sort-Object

    if ($isWindows) {
        $payloadRoot = ""
        $logicalEntries = $normalizedEntries
    } else {
        $payloadRoot = "xana-$Target"
        $prefix = "$payloadRoot/"
        $logicalEntries = @($normalizedEntries | ForEach-Object {
            if (-not $_.StartsWith($prefix, [StringComparison]::Ordinal)) {
                throw "Unix archive entry is outside the canonical payload root ${payloadRoot}: $_"
            }
            $_.Substring($prefix.Length)
        })
    }

    $actualEntries = @($logicalEntries | Sort-Object)
    $difference = @(Compare-Object $actualEntries $expectedEntries -CaseSensitive)
    if ($difference.Count -ne 0) {
        throw "archive inventory differs from the bounded release contract: $($normalizedEntries -join ', ')"
    }

    [pscustomobject]@{
        PayloadRoot = $payloadRoot
        Entries = $actualEntries
    }
}
