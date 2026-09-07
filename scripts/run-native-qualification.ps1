param(
    [Parameter(Mandatory)]
    [ValidateSet('format', 'contracts', 'lint', 'all-features', 'no-default', 'root-no-default', 'custody', 'source-package', 'resources')]
    [string]$Check
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/native-qualification.ps1"

Push-Location (Split-Path -Parent $PSScriptRoot)
$report = $null
try {
    $output = 'target/native-qualification'
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    $revision = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'cannot identify qualification revision' }
    $toolchain = & rustc --version
    if ($LASTEXITCODE -ne 0) { throw 'cannot identify qualification toolchain' }
    $worktreeStatus = @(& git status --porcelain)
    if ($LASTEXITCODE -ne 0) { throw 'cannot identify qualification worktree state' }
    $report = [ordered]@{
        check = $Check; commit = "$revision"; toolchain = "$toolchain"
        worktree_dirty = $worktreeStatus.Count -gt 0
        lock_sha256 = (Get-FileHash Cargo.lock -Algorithm SHA256).Hash.ToLowerInvariant()
        os = [Runtime.InteropServices.RuntimeInformation]::OSDescription
        architecture = "$( [Runtime.InteropServices.RuntimeInformation]::OSArchitecture )"
        logical_processors = [Environment]::ProcessorCount
        runtime_available_memory_bytes = [GC]::GetGCMemoryInfo().TotalAvailableMemoryBytes
        runner_image = "$env:ImageOS $env:ImageVersion"
        run_id = $env:GITHUB_RUN_ID; attempt = $env:GITHUB_RUN_ATTEMPT
        started_utc = [DateTime]::UtcNow.ToString('o'); status = 'failed'
        tests = $null
        limitations = @('No Unix browser qualification', 'No interactive OS consent or GUI/FPS evidence', 'No real-account or sleep/login validation')
    }
    if ($Check -eq 'source-package') {
        Invoke-NativeQualificationCommand pwsh @('-NoProfile', '-File', 'scripts/check-package-contents.ps1') "$output/package.log"
        Invoke-NativeQualificationCommand pwsh @('-NoProfile', '-File', 'scripts/test-package-contents.ps1') "$output/package-fixtures.log"
        # Install only from the locked repository, retaining the SQLCipher patch.
        $installRoot = Join-Path ([IO.Path]::GetTempPath()) ("xana-native-install-" + [guid]::NewGuid().ToString('N'))
        Invoke-NativeQualificationCommand cargo @('install', '--locked', '--path', '.', '--debug', '--root', $installRoot) "$output/install.log"
        Invoke-NativeQualificationCommand (Join-Path $installRoot 'bin/xana') @('--version') "$output/installed-version.log"
        # On hosted runners the disposable VM reclaims the install; no recursive
        # deletion of computed paths and no upload of the installation tree.
    } else {
        $command = Get-NativeQualificationCommand $Check
        $report['command'] = @($command.Program) + $command.Arguments
        Invoke-NativeQualificationCommand $command.Program $command.Arguments "$output/$Check.log"
        if ($Check -in @('all-features', 'no-default', 'root-no-default', 'custody')) {
            $report.tests = Get-NativeTestEvidence (Get-Content -Raw -LiteralPath "$output/$Check.log") -Custody:($Check -eq 'custody')
        }
    }
    $report.status = 'passed'
} finally {
    if ($null -ne $report) {
        $report['finished_utc'] = [DateTime]::UtcNow.ToString('o')
        $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$output/$Check.json" -Encoding utf8
        if ($env:GITHUB_STEP_SUMMARY) {
            "- **$Check**: $($report.status), commit $($report.commit). See the native-qualification artifact; native browser and human gates remain separate." |
                Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8
        }
    }
    Pop-Location
}
