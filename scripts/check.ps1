# Checks for the Queqiao workspace: cargo fmt, clippy, build, test, then runs each program once.
# Usage on the PC: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
# Prints one line per step and CHECK_PASS or CHECK_FAIL <step> last; full cargo output in target\check.log.

# Native tools write progress to stderr. Under 'Stop', Windows PowerShell 5.1 turns that into a fatal error.
$ErrorActionPreference = 'Continue'
$Root = Split-Path -Parent $PSScriptRoot
$Cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$Log = Join-Path $Root 'target\check.log'
Set-Location -LiteralPath $Root
New-Item -ItemType Directory -Force (Join-Path $Root 'target') | Out-Null
Set-Content -LiteralPath $Log -Value "check $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')" -Encoding utf8

# OpenH264 (the software encoder) builds without NASM, silently, and then runs about 5x slower (measured:
# 35 ms instead of 6.6 ms per 1080p frame). Builds for release must have NASM on the PATH.
if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) {
    'WARNING  nasm not on PATH: OpenH264 is built without its assembly (about 5x slower)'
}

# libopus (sound) is built from source with CMake. Visual Studio's Build Tools include CMake but do not put it
# on the PATH: use theirs when there is no other (the change lasts for this PowerShell process).
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vsRoot = if (Test-Path -LiteralPath $vswhere) { & $vswhere -latest -products * -property installationPath } else { $null }
    $vsCmake = if ($vsRoot) { Join-Path $vsRoot 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin' } else { $null }
    if ($vsCmake -and (Test-Path -LiteralPath (Join-Path $vsCmake 'cmake.exe'))) {
        $env:Path = "$vsCmake;$env:Path"
    } else {
        'WARNING  cmake not found: libopus (sound) cannot be built'
    }
}

$failedStep = $null

# Windows PowerShell 5.1 reads a script without a byte-order mark in the system code page (GBK on a Chinese
# Windows): non-ASCII text in it can swallow the closing quote and break the script there only (found 2026-11-21).
# Scripts stay ASCII, or carry a UTF-8 byte-order mark.
$scriptProblems = @(Get-ChildItem -LiteralPath (Join-Path $Root 'scripts'), (Join-Path $Root 'tools') -Filter '*.ps1' -ErrorAction SilentlyContinue | Where-Object {
    $bytes = [IO.File]::ReadAllBytes($_.FullName)
    $marked = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    -not $marked -and @($bytes | Where-Object { $_ -ge 0x80 }).Count -gt 0
})
'{0,-8} {1}' -f 'scripts', $(if ($scriptProblems.Count) { 'non-ASCII without a byte-order mark: ' + (($scriptProblems | ForEach-Object Name) -join ', ') } else { 'ascii ok' })
if ($scriptProblems.Count) { $failedStep = 'scripts' }

$steps = @(
    @{ Name = 'fmt'; CargoArgs = @('fmt', '--all', '--check') },
    @{ Name = 'clippy'; CargoArgs = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
    @{ Name = 'build'; CargoArgs = @('build', '--workspace', '--all-targets') },
    @{ Name = 'test'; CargoArgs = @('test', '--workspace') }
)
foreach ($step in $steps) {
    $started = Get-Date
    "== cargo $($step.CargoArgs -join ' ')" | Add-Content -LiteralPath $Log -Encoding utf8
    & $Cargo $step.CargoArgs 2>&1 | ForEach-Object { "$_" } | Add-Content -LiteralPath $Log -Encoding utf8
    $exitCode = $LASTEXITCODE
    '{0,-8} exit {1}  {2:0.0} s' -f $step.Name, $exitCode, ((Get-Date) - $started).TotalSeconds
    if ($exitCode -ne 0) { $failedStep = $step.Name; break }
}

# Every crate with a src\main.rs is a program: run it with --version and expect its banner
# "<name> <version> ...". Without options the agent goes online and the viewer opens its login form, so
# never start them bare here; a program still running after 20 s is stopped and fails the check.
if (-not $failedStep) {
    $programs = Get-ChildItem -LiteralPath (Join-Path $Root 'crates') -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'src\main.rs') }
    foreach ($program in $programs) {
        $exe = Join-Path $Root "target\debug\$($program.Name).exe"
        $startInfo = New-Object Diagnostics.ProcessStartInfo $exe, '--version'
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $process = [Diagnostics.Process]::Start($startInfo)
        if ($process.WaitForExit(20000)) {
            $output = @(($process.StandardOutput.ReadToEnd() + $process.StandardError.ReadToEnd()) -split "`r?`n" | Where-Object { $_ })
            $exitCode = $process.ExitCode
        } else {
            $process.Kill()
            $output = @('still running after 20 s')
            $exitCode = 'timeout'
        }
        '{0,-12} exit {1}  {2}' -f $program.Name, $exitCode, ($output -join ' | ')
        if ($exitCode -ne 0 -or -not ($output[0] -like "$($program.Name) *")) { $failedStep = "run $($program.Name)" }
    }
}

if ($failedStep) { "CHECK_FAIL $failedStep (log: $Log)" } else { "CHECK_PASS (log: $Log)" }
