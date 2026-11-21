# Makes the installer from the release build: signs queqiao-viewer.exe, queqiao-agent.exe and queqiao-service.exe
# (scripts\sign.ps1), packs them into queqiao-setup.exe (queqiao-setup pack), signs that too, and prints its SHA-256.
# Output: target\package\queqiao-setup-<version>.exe. For now the signature is the test certificate's.
# Usage: scripts\package.ps1 [-Bin <release folder, default target\release>] [-Subject 'CN=Queqiao Test Signing'] [-NoTimestamp]
# Prints PACKAGE_PASS <file> <sha256> or PACKAGE_FAIL <reason>. ASCII only.
param([string]$Bin = '', [string]$Subject = 'CN=Queqiao Test Signing', [switch]$NoTimestamp)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$release = if ($Bin) { $Bin } else { Join-Path $root 'target\release' }
$programs = @('queqiao-viewer.exe', 'queqiao-agent.exe', 'queqiao-service.exe') | ForEach-Object { Join-Path $release $_ }
foreach ($program in $programs + (Join-Path $release 'queqiao-setup.exe')) {
    if (-not (Test-Path -LiteralPath $program)) { "PACKAGE_FAIL $program is missing (build the release first)"; return }
}
$version = ((& (Join-Path $release 'queqiao-agent.exe') --version 2>$null | Select-Object -First 1) -split ' ')[1]
if (-not $version) { $version = '0.0.0' }
$signArgs = @{ Subject = $Subject }
if ($NoTimestamp) { $signArgs.NoTimestamp = $true }

$signed = @(& (Join-Path $PSScriptRoot 'sign.ps1') -Files $programs @signArgs)
$signed | Where-Object { $_ -notmatch '^certificate:' }
if ($signed[-1] -ne 'SIGN_PASS') { 'PACKAGE_FAIL signing the programs'; return }

$out = Join-Path $root 'target\package'
New-Item -ItemType Directory -Force $out | Out-Null
$installer = Join-Path $out "queqiao-setup-$version.exe"
Remove-Item -LiteralPath $installer -ErrorAction SilentlyContinue
$packed = @(& (Join-Path $release 'queqiao-setup.exe') pack --output $installer @programs 2>&1 | ForEach-Object { "$_" })
$packed
if (-not (Test-Path -LiteralPath $installer)) { 'PACKAGE_FAIL packing'; return }

$signedSetup = @(& (Join-Path $PSScriptRoot 'sign.ps1') -Files $installer @signArgs)
$signedSetup | Where-Object { $_ -notmatch '^certificate:' }
if ($signedSetup[-1] -ne 'SIGN_PASS') { 'PACKAGE_FAIL signing the installer'; return }
"PACKAGE_PASS $installer $((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash)"
