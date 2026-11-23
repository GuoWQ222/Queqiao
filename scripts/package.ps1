# Makes the installer from the release build: signs queqiao-viewer.exe, queqiao-agent.exe and queqiao-service.exe
# (scripts\sign.ps1), packs them into queqiao-setup.exe (queqiao-setup pack), signs that too, and prints its SHA-256.
# Output: target\package\queqiao-setup-<version>.exe. For now the signature is the test certificate's.
# -Ship: a shipping build first (target\ship, a plain cargo build: without the options for the tests, which the test
# build target\release has), checked to refuse those options; anything meant for people must be made so.
# Usage: scripts\package.ps1 [-Ship | -Bin <release folder, default target\release>] [-Subject 'CN=Queqiao Test Signing'] [-NoTimestamp]
# Prints PACKAGE_PASS <file> <sha256> or PACKAGE_FAIL <reason>. ASCII only.
param([string]$Bin = '', [switch]$Ship, [string]$Subject = 'CN=Queqiao Test Signing', [switch]$NoTimestamp)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
if ($Ship -and $Bin) { 'PACKAGE_FAIL -Ship builds its own programs: no -Bin with it'; return }
if ($Ship) {
    # libopus is built with CMake: Visual Studio's, as in scripts\check.ps1, when none is on the PATH.
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $vsRoot = if (Test-Path -LiteralPath $vswhere) { & $vswhere -latest -products * -property installationPath } else { $null }
        if ($vsRoot) { $env:Path = (Join-Path $vsRoot 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin') + ";$env:Path" }
    }
    if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) { 'WARNING nasm not on PATH: OpenH264 is built without its assembly (about 5x slower)' }
    $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
    $built = @(& $cargo build --release --target-dir (Join-Path $root 'target\ship') -p queqiao-agent -p queqiao-viewer -p queqiao-service -p queqiao-setup 2>&1 | ForEach-Object { "$_" })
    if ($LASTEXITCODE) { $built | Select-Object -Last 15; 'PACKAGE_FAIL the shipping build'; return }
    $Bin = Join-Path $root 'target\ship\release'
    # Each program must refuse an option for the tests at once (exit code 2) instead of acting on it.
    foreach ($probe in @(@('queqiao-agent.exe', '--listen 127.0.0.1:9'), @('queqiao-viewer.exe', '--connect 127.0.0.1:9'))) {
        $process = Start-Process -FilePath (Join-Path $Bin $probe[0]) -ArgumentList $probe[1] -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id -Force; "PACKAGE_FAIL $($probe[0]) $($probe[1]) was not refused"; return }
        if ($process.ExitCode -ne 2) { "PACKAGE_FAIL $($probe[0]) $($probe[1]): exit code $($process.ExitCode), not 2"; return }
        "refused as it should: $($probe[0]) $($probe[1])"
    }
    # The service reads all its options before it does anything: the unknown last one stops even a test build, and
    # the message says which option it refused.
    $said = "$(& (Join-Path $Bin 'queqiao-service.exe') install --agent-arguments x --never-an-option 2>&1)"
    if ($said -notmatch 'unknown option --agent-arguments') { "PACKAGE_FAIL queqiao-service.exe took --agent-arguments ($said)"; return }
    'refused as it should: queqiao-service.exe install --agent-arguments'
}
$release = if ($Bin) { $Bin } else { Join-Path $root 'target\release' }
$programs = @('queqiao-viewer.exe', 'queqiao-agent.exe', 'queqiao-service.exe') | ForEach-Object { Join-Path $release $_ }
foreach ($program in $programs + (Join-Path $release 'queqiao-setup.exe')) {
    if (-not (Test-Path -LiteralPath $program)) { "PACKAGE_FAIL $program is missing (build the release first)"; return }
}
# The licence and the third-party notices go with the programs (the notices' licences ask for them in every copy);
# text, so packed as they are, not signed.
$texts = @('LICENSE', 'THIRD_PARTY_NOTICES.md') | ForEach-Object { Join-Path $root $_ }
foreach ($text in $texts) {
    if (-not (Test-Path -LiteralPath $text)) { "PACKAGE_FAIL $text is missing"; return }
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
$packed = @(& (Join-Path $release 'queqiao-setup.exe') pack --output $installer @programs @texts 2>&1 | ForEach-Object { "$_" })
$packed
if (-not (Test-Path -LiteralPath $installer)) { 'PACKAGE_FAIL packing'; return }

$signedSetup = @(& (Join-Path $PSScriptRoot 'sign.ps1') -Files $installer @signArgs)
$signedSetup | Where-Object { $_ -notmatch '^certificate:' }
if ($signedSetup[-1] -ne 'SIGN_PASS') { 'PACKAGE_FAIL signing the installer'; return }
"PACKAGE_PASS $installer $((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash)"
