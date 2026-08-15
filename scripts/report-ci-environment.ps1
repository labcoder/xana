$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$os = [Runtime.InteropServices.RuntimeInformation]::OSDescription
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$processors = [Environment]::ProcessorCount
$temporaryRoot = [IO.Path]::GetTempPath()
$runnerImage = if ([string]::IsNullOrWhiteSpace($env:ImageOS)) {
    "local"
} else {
    "$($env:ImageOS) $($env:ImageVersion)"
}
$windowsPowerShell = Get-Command powershell.exe -ErrorAction SilentlyContinue
$powerShellCore = Get-Command pwsh -ErrorAction SilentlyContinue
$bash = Get-Command bash -ErrorAction SilentlyContinue

Write-Output "OS: $os ($architecture)"
Write-Output "Runner image: $runnerImage"
Write-Output "Logical processors: $processors"
Write-Output "Temporary root: $temporaryRoot"
Write-Output "PowerShell Core: $($PSVersionTable.PSVersion) ($($powerShellCore.Source))"
if ($null -ne $windowsPowerShell) {
    Write-Output "Windows PowerShell: $($windowsPowerShell.Source)"
}
if ($null -ne $bash) {
    Write-Output "Default Bash: $($bash.Source)"
}
& rustc --version --verbose
if ($LASTEXITCODE -ne 0) { throw "rustc version probe failed" }
& cargo --version
if ($LASTEXITCODE -ne 0) { throw "cargo version probe failed" }
