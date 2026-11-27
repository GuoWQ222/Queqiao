# Makes the installer from the release build: signs queqiao-viewer.exe, queqiao-agent.exe and queqiao-service.exe
# (scripts\sign.ps1), packs them into queqiao-setup.exe (queqiao-setup pack), signs that too, and prints its SHA-256.
# Output: target\package\queqiao-setup-<version>.exe. For now the signature is the test certificate's.
# -Ship: a shipping build first (target\ship, a plain cargo build: without the options for the tests, which the test
# build target\release has), checked to refuse those options; anything meant for people must be made so.
# -Server host:port -Pin <SHA-256 hex>: the account server the login form offers (built in; the person can still type
# another), with its certificate's fingerprint; with -Ship only. Not secret (every program that connects shows it),
# but given here rather than kept in the public repository.
# Usage: scripts\package.ps1 [-Ship [-Server <host:port> -Pin <sha256>] | -Bin <release folder, default target\release>] [-Subject 'CN=Queqiao Test Signing'] [-NoTimestamp]
# Prints PACKAGE_PASS <file> <sha256> or PACKAGE_FAIL <reason>. ASCII only.
param([string]$Bin = '', [switch]$Ship, [string]$Server = '', [string]$Pin = '', [string]$Subject = 'CN=Queqiao Test Signing', [switch]$NoTimestamp)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
if ($Ship -and $Bin) { 'PACKAGE_FAIL -Ship builds its own programs: no -Bin with it'; return }
if (($Server -or $Pin) -and -not $Ship) { 'PACKAGE_FAIL -Server and -Pin go with -Ship'; return }
if (($Server -and -not $Pin) -or ($Pin -and -not $Server)) { 'PACKAGE_FAIL -Server and -Pin go together'; return }
if ($Server -and $Server -notmatch '^(\[[0-9A-Fa-f:]+\]|[A-Za-z0-9.-]+):\d{1,5}$') { "PACKAGE_FAIL -Server $Server is not host:port"; return }
if ($Pin -and $Pin -notmatch '^[0-9a-f]{64}$') { 'PACKAGE_FAIL -Pin is not a SHA-256 fingerprint (64 lowercase hex digits)'; return }
if ($Ship) {
    # libopus is built with CMake: Visual Studio's, as in scripts\check.ps1, when none is on the PATH.
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $vsRoot = if (Test-Path -LiteralPath $vswhere) { & $vswhere -latest -products * -property installationPath } else { $null }
        if ($vsRoot) { $env:Path = (Join-Path $vsRoot 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin') + ";$env:Path" }
    }
    if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) { 'WARNING nasm not on PATH: OpenH264 is built without its assembly (about 5x slower)' }
    $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
    # The build machine's folders out of the programs: Rust keeps each library's source path for its error messages
    # (C:\Users\<name>\.cargo\registry\src\<mirror>\...: the builder's user name and mirror, the security review of
    # 2026-11-26). Rewritten to "cargo\..." and "rustup\..."; separated by 0x1F, so a path with spaces stays whole.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$env:USERPROFILE\.cargo" }
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { "$env:USERPROFILE\.rustup" }
    $remap = @("--remap-path-prefix=$cargoHome=cargo", "--remap-path-prefix=$rustupHome=rustup", "--remap-path-prefix=$root=queqiao")
    $env:CARGO_ENCODED_RUSTFLAGS = $remap -join [char]0x1F
    # The C libraries keep their files' paths too (libopus' checks: 260 of them in the agent): MSVC trims the start
    # of those with /d1trimfile. The flags are split at spaces, so a Cargo folder with a space in it cannot be given
    # (the check below then says so).
    $cflags, $cxxflags = $env:CFLAGS, $env:CXXFLAGS
    if ($cargoHome -notmatch ' ') {
        $env:CFLAGS = "$cflags /d1trimfile:$cargoHome\".Trim()
        $env:CXXFLAGS = "$cxxflags /d1trimfile:$cargoHome\".Trim()
    }
    # The built-in server (queqiao-account's DEFAULT_SERVER / DEFAULT_PIN; cargo builds again when they change). Set
    # either way, so one from the environment never slips in unasked.
    $env:QUEQIAO_DEFAULT_SERVER, $env:QUEQIAO_DEFAULT_PIN = $Server, $Pin
    if (-not $Server) { Remove-Item Env:\QUEQIAO_DEFAULT_SERVER, Env:\QUEQIAO_DEFAULT_PIN -ErrorAction SilentlyContinue }
    "server built in: $(if ($Server) { "$Server (pin $($Pin.Substring(0, 16))...)" } else { 'none (typed in the login form)' })"
    $built = @(& $cargo build --release --target-dir (Join-Path $root 'target\ship') -p queqiao-agent -p queqiao-viewer -p queqiao-service -p queqiao-setup 2>&1 | ForEach-Object { "$_" })
    Remove-Item Env:\CARGO_ENCODED_RUSTFLAGS, Env:\QUEQIAO_DEFAULT_SERVER, Env:\QUEQIAO_DEFAULT_PIN -ErrorAction SilentlyContinue
    $env:CFLAGS, $env:CXXFLAGS = $cflags, $cxxflags
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
    # Nothing of the build machine in what ships: its user folder, Cargo's and rustup's (as text or UTF-16).
    foreach ($exe in 'queqiao-agent.exe', 'queqiao-viewer.exe', 'queqiao-service.exe', 'queqiao-setup.exe') {
        $bytes = [IO.File]::ReadAllBytes((Join-Path $Bin $exe))
        $decoded = @([Text.Encoding]::ASCII.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes))
        foreach ($folder in @($env:USERPROFILE, $cargoHome, $rustupHome) | Where-Object { $_ }) {
            if (@($decoded | Where-Object { $_.IndexOf($folder, [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count) {
                "PACKAGE_FAIL $exe holds the build machine's folder $folder"
                return
            }
        }
    }
    'no build machine folders in the programs'
    # The test key is never trusted by what ships (security review 2026-11-27): its private half lies on the build PC.
    $libText = Get-Content -Raw -LiteralPath (Join-Path $root 'crates\queqiao-update\src\lib.rs')
    $testKey = [regex]::Match($libText, 'pub const TEST_KEY: &str = "([0-9a-f]{64})"').Groups[1].Value
    $releaseKey = [regex]::Match($libText, 'pub const RELEASE_KEY: &str = "([0-9a-f]{64})"').Groups[1].Value
    if (-not $testKey -or -not $releaseKey) { 'PACKAGE_FAIL TEST_KEY or RELEASE_KEY not found in queqiao-update'; return }
    foreach ($exe in 'queqiao-agent.exe', 'queqiao-service.exe', 'queqiao-setup.exe') {
        $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $Bin $exe)))
        if ($text.IndexOf($testKey) -ge 0) { "PACKAGE_FAIL $exe holds the test key"; return }
        if ($text.IndexOf($releaseKey) -lt 0) { "PACKAGE_FAIL $exe does not hold the release key"; return }
    }
    if ($releaseKey -match '^0+$') { 'release key: none yet (placeholder): these programs take no update' } else { "release key: $($releaseKey.Substring(0, 16))... (not the test key)" }
    # The server asked for is in the programs that log in.
    if ($Server) {
        foreach ($exe in 'queqiao-agent.exe', 'queqiao-viewer.exe') {
            $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $Bin $exe)))
            if ($text.IndexOf($Server) -lt 0 -or $text.IndexOf($Pin) -lt 0) { "PACKAGE_FAIL $exe does not have the server $Server built in"; return }
        }
        "server built in (agent, viewer): $Server"
    }
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
