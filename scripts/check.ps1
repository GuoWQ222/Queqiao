# Checks for the RemoteControl workspace: cargo fmt, clippy, build, test, then runs each program once.
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

$failedStep = $null
$steps = @(
    @{ Name = 'fmt'; CargoArgs = @('fmt', '--all', '--check') },
    @{ Name = 'clippy'; CargoArgs = @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') },
    @{ Name = 'build'; CargoArgs = @('build', '--workspace') },
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

# Every crate with a src\main.rs is a program: run it once and expect its banner "<name> <version> ...".
if (-not $failedStep) {
    $programs = Get-ChildItem -LiteralPath (Join-Path $Root 'crates') -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'src\main.rs') }
    foreach ($program in $programs) {
        $exe = Join-Path $Root "target\debug\$($program.Name).exe"
        $output = @(& $exe 2>&1 | ForEach-Object { "$_" })
        $exitCode = $LASTEXITCODE
        '{0,-12} exit {1}  {2}' -f $program.Name, $exitCode, ($output -join ' | ')
        if ($exitCode -ne 0 -or -not ($output[0] -like "$($program.Name) *")) { $failedStep = "run $($program.Name)" }
    }
}

if ($failedStep) { "CHECK_FAIL $failedStep (log: $Log)" } else { "CHECK_PASS (log: $Log)" }
