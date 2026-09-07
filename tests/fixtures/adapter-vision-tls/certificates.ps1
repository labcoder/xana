param([switch]$Renew)

$ErrorActionPreference = 'Stop'

if ($Renew) {
    # In-memory, public test keys only. Never install these in an OS trust store.
    $rootKey = [System.Security.Cryptography.RSA]::Create(2048)
    $serverKey = [System.Security.Cryptography.RSA]::Create(2048)
    $root = $null
    $server = $null
    try {
        $hash = [System.Security.Cryptography.HashAlgorithmName]::SHA256
        $padding = [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
        $start = [DateTimeOffset]::UtcNow.AddDays(-1)
        $rootRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
            'CN=Xana TEST ONLY vision fixture CA', $rootKey, $hash, $padding)
        $rootRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true, $false, 0, $true))
        $rootRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
                [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign, $true))
        $rootRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509SubjectKeyIdentifierExtension]::new($rootRequest.PublicKey, $false))
        $root = $rootRequest.CreateSelfSigned($start, $start.AddYears(10))

        $serverRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
            'CN=localhost', $serverKey, $hash, $padding)
        $serverRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false, $false, 0, $true))
        $serverRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
                [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature, $true))
        $purposes = [System.Security.Cryptography.OidCollection]::new()
        [void]$purposes.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
        $serverRequest.CertificateExtensions.Add(
            [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($purposes, $false))
        $names = [System.Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
        $names.AddDnsName('localhost')
        $names.AddIpAddress([System.Net.IPAddress]::Loopback)
        $serverRequest.CertificateExtensions.Add($names.Build())
        $serial = [byte[]]::new(16)
        [System.Security.Cryptography.RandomNumberGenerator]::Fill($serial)
        $server = $serverRequest.Create($root, $start, $start.AddDays(397), $serial)

        $fixtures = @{
            'test-ca.der.b64' = $root.RawData
            'test-server.der.b64' = $server.RawData
            'test-server-key.der.b64' = $serverKey.ExportPkcs8PrivateKey()
        }
        foreach ($name in $fixtures.Keys) {
            [System.IO.File]::WriteAllText((Join-Path $PSScriptRoot $name),
                [Convert]::ToBase64String($fixtures[$name]) + "`n", [System.Text.UTF8Encoding]::new($false))
        }
    }
    finally {
        if ($null -ne $server) { $server.Dispose() }
        if ($null -ne $root) { $root.Dispose() }
        $serverKey.Dispose()
        $rootKey.Dispose()
    }
}

$bytes = [Convert]::FromBase64String([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot 'test-server.der.b64')).Trim())
$certificate = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($bytes)
try {
    $eku = $certificate.Extensions | Where-Object { $_.Oid.Value -eq '2.5.29.37' }
    if ($null -eq $eku -or '1.3.6.1.5.5.7.3.1' -notin $eku.EnhancedKeyUsages.Value) {
        throw 'loopback TLS fixture requires the serverAuth extended key usage'
    }
    if (($certificate.NotAfter - $certificate.NotBefore).TotalDays -gt 398) {
        throw 'loopback TLS fixture validity must be at most 398 days for native validation'
    }
    if ($certificate.NotBefore.ToUniversalTime() -gt [DateTime]::UtcNow -or
        $certificate.NotAfter.ToUniversalTime() -lt [DateTime]::UtcNow.AddDays(14)) {
        throw 'loopback TLS fixture is not current; renew and review all three fixtures with certificates.ps1 -Renew'
    }
    Write-Output "loopback TLS fixture: serverAuth present; bounded validity; expires $($certificate.NotAfter.ToUniversalTime().ToString('yyyy-MM-dd'))"
}
finally { $certificate.Dispose() }
