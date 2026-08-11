function Test-SafeFixtureCleanupRoot {
    param(
        [Parameter(Mandatory)][string]$Parent,
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][char]$Separator,
        [Parameter(Mandatory)][string]$RequiredPrefix
    )

    $parentPrefix = $Parent.TrimEnd([char[]]@('/', '\')) + [string]$Separator
    if (-not $Root.StartsWith($parentPrefix, [StringComparison]::Ordinal)) {
        return $false
    }
    $relative = $Root.Substring($parentPrefix.Length)
    return $relative.StartsWith($RequiredPrefix, [StringComparison]::Ordinal) -and
        $relative.IndexOfAny([char[]]@('/', '\')) -lt 0
}

function Assert-FixtureCleanupGuardContract {
    $cases = @(
        @("/var/folders/ab/cd/T/", "/var/folders/ab/cd/T/xana-fixture-0123", '/', $true),
        @("C:\Temp\", "C:\Temp\xana-fixture-0123", '\', $true),
        @("/tmp/", "/tmp/nested/xana-fixture-0123", '/', $false)
    )
    foreach ($case in $cases) {
        $actual = Test-SafeFixtureCleanupRoot `
            -Parent $case[0] `
            -Root $case[1] `
            -Separator $case[2] `
            -RequiredPrefix "xana-fixture-"
        if ($actual -ne $case[3]) {
            throw "fixture cleanup guard regression"
        }
    }
}
