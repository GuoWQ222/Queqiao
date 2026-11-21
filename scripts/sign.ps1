# Signs Queqiao's programs (Authenticode, SHA-256, with an RFC 3161 time stamp) with the code-signing certificate
# in the current user's store whose subject is -Subject. For now that is a self-signed TEST certificate (the user's
# choice 2026-11-21): a real certificate later replaces it and nothing else changes.
#   -CreateTest      makes the test certificate if there is none (3072-bit RSA, key not exportable, 2 years)
#   -TrustOnThisPC   adds its public part to this PC's Trusted Root and Trusted Publishers: TEST MACHINES ONLY
#                    (a PC that trusts it accepts anything signed with it)
#   -NoTimestamp     signs without the time stamp server (offline)
# Then checks each signature (signtool verify /pa): "chain trusted" is yes only where the certificate is trusted.
# Usage: scripts\sign.ps1 [-Files <file>...] [-Subject 'CN=Queqiao Test Signing'] [-CreateTest] [-TrustOnThisPC]
# Prints SIGN_PASS or SIGN_FAIL <reason>. ASCII only.
param(
    [string[]]$Files = @(),
    [string]$Subject = 'CN=Queqiao Test Signing',
    [switch]$CreateTest,
    [switch]$TrustOnThisPC,
    [switch]$NoTimestamp,
    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Files) {
    $Files = @(Get-ChildItem -LiteralPath (Join-Path $root 'target\release') -Filter 'queqiao-*.exe' | ForEach-Object FullName)
}
if (-not $Files) { 'SIGN_FAIL nothing to sign (build the release first)'; return }

# signtool of the newest Windows SDK on this PC.
$kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$signtool = Get-ChildItem -LiteralPath $kits -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
    Where-Object { $_.Directory.Name -eq 'x64' } | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { 'SIGN_FAIL signtool.exe not found (Windows SDK)'; return }

$certificate = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object { $_.Subject -eq $Subject -and $_.NotAfter -gt (Get-Date) -and $_.HasPrivateKey } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $certificate -and $CreateTest) {
    $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject -KeyAlgorithm RSA -KeyLength 3072 `
        -HashAlgorithm SHA256 -KeyExportPolicy NonExportable -CertStoreLocation Cert:\CurrentUser\My `
        -NotAfter (Get-Date).AddYears(2)
    "made the test certificate $($certificate.Thumbprint) (key not exportable, until $($certificate.NotAfter.ToString('yyyy-MM-dd')))"
}
if (-not $certificate) { "SIGN_FAIL no code-signing certificate '$Subject' with its key in CurrentUser\My (-CreateTest makes a test one)"; return }
"certificate: $($certificate.Subject), $($certificate.Thumbprint), until $($certificate.NotAfter.ToString('yyyy-MM-dd'))"

if ($TrustOnThisPC) {
    $public = Join-Path $env:TEMP 'queqiao_test_signing.cer'
    Export-Certificate -Cert $certificate -FilePath $public -Type CERT | Out-Null
    foreach ($store in 'Cert:\LocalMachine\Root', 'Cert:\LocalMachine\TrustedPublisher') {
        Import-Certificate -FilePath $public -CertStoreLocation $store | Out-Null
        "trusted on this PC: $store"
    }
    Remove-Item -LiteralPath $public -ErrorAction SilentlyContinue
}

$failed = @()
foreach ($file in $Files) {
    $arguments = @('sign', '/fd', 'SHA256', '/sha1', $certificate.Thumbprint)
    if (-not $NoTimestamp) { $arguments += @('/tr', $TimestampUrl, '/td', 'SHA256') }
    $output = @(& $signtool.FullName @arguments $file 2>&1 | ForEach-Object { "$_" })
    if ($LASTEXITCODE) {
        $failed += [IO.Path]::GetFileName($file)
        "sign failed: $file"
        $output | Select-Object -Last 3
        continue
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $file
    $null = & $signtool.FullName verify /pa /q $file 2>&1
    $trusted = $LASTEXITCODE -eq 0
    '{0,-28} signed by {1}; time stamp: {2}; chain trusted here: {3}' -f [IO.Path]::GetFileName($file),
        $signature.SignerCertificate.Subject,
        $(if ($signature.TimeStamperCertificate) { 'yes' } else { 'no' }),
        $(if ($trusted) { 'yes' } else { 'no' })
    if ($signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) { $failed += [IO.Path]::GetFileName($file) }
}
if ($failed.Count) { "SIGN_FAIL $($failed -join ', ')" } else { 'SIGN_PASS' }
