# Runs the M1 local loop (loopback.exe: capture -> encode -> decode -> window) in the desktop session for
# the main paths and prints the reports. A small window appears for each run and closes by itself.
# Copy this script, run_in_session.ps1 and loopback.exe (release) into one folder on a test machine, or point
# -Bin at target\release\examples.
# Usage: scripts\run_loopbacks.ps1 [-Bin <folder>] [-Out <report folder>] [-Seconds 8] [-Only <name,...>]
param(
    [string]$Bin = $PSScriptRoot,
    [string]$Out = (Join-Path $PSScriptRoot 'loopback_out'),
    [int]$Seconds = 8,
    [string[]]$Only = @()
)
$ErrorActionPreference = 'Continue'
$session = Join-Path $PSScriptRoot 'run_in_session.ps1'
$runs = [ordered]@{
    'auto' = ''
    'software' = 'force=encode:software,decode:software'
    'cpu_convert' = 'force=convert:cpu'
    'warp' = 'force=adapter:warp'
}
foreach ($name in $runs.Keys) {
    if ($Only.Count -gt 0 -and $name -notin $Only) { continue }
    $dir = Join-Path $Out $name
    if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
    $arguments = ('"{0}" seconds={1} {2}' -f $dir, $Seconds, $runs[$name]).Trim()
    "=== $name"
    & $session -Exe (Join-Path $Bin 'loopback.exe') -Arguments $arguments -TimeoutSec ($Seconds + 60)
    $report = Join-Path $dir 'loopback.txt'
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report -Encoding UTF8 } else { "no report at $report" }
}
