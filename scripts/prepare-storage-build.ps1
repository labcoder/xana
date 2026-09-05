# CI-only preparation; no machine-level installation or execution-policy change.
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { return }
if (-not $env:RUNNER_TEMP -or -not $env:GITHUB_ENV -or -not $env:GITHUB_PATH) {
    throw 'This helper is for GitHub runners. Locally install complete Perl and NASM; see the installation guide.'
}
$perl = 'C:\Strawberry\perl\bin\perl.exe'
if (-not (Test-Path -LiteralPath $perl -PathType Leaf)) {
    throw 'The Windows runner is missing the required complete Strawberry Perl installation.'
}
& $perl -e 'use FindBin; use IPC::Cmd; print "Perl native-build prerequisites available\n";'
if ($LASTEXITCODE -ne 0) { throw 'Perl is incomplete; refusing to run the native build.' }
"OPENSSL_SRC_PERL=$perl" >> $env:GITHUB_ENV
$toolsDirectory = Join-Path $env:RUNNER_TEMP ('xana-native-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $toolsDirectory | Out-Null
$archive = Join-Path $toolsDirectory 'nasm.zip'
Invoke-WebRequest -Uri 'https://www.nasm.us/pub/nasm/releasebuilds/3.02/win64/nasm-3.02-win64.zip' -OutFile $archive
$expected = '161d0bfaff53c2f9e9f3e69fd0672323ebabafd1268976a5cec11be92a19aee7'
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expected) {
    throw 'NASM archive checksum mismatch; downloaded code was not executed.'
}
Expand-Archive -LiteralPath $archive -DestinationPath $toolsDirectory
$nasmDirectory = Join-Path $toolsDirectory 'nasm-3.02'
$nasm = Join-Path $nasmDirectory 'nasm.exe'
if (-not (Test-Path -LiteralPath $nasm -PathType Leaf)) { throw 'Verified NASM archive has an unexpected layout.' }
& $nasm -v
if ($LASTEXITCODE -ne 0) { throw 'Verified NASM could not run.' }
$nasmDirectory >> $env:GITHUB_PATH
