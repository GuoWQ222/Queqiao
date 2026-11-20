# Runs the M1 probes in the logged-on user's desktop session and prints their reports. Used on the
# development PC and on any test machine: copy this script, run_in_session.ps1, capture_probe.exe and
# encode_probe.exe (release builds) into one folder and run it there.
# Usage: scripts\run_probes.ps1 [-Bin <folder with the .exe files>] [-Out <report folder>] [-Only <name,...>]
# Runs: capture, encode_auto (best encoder the machine has), encode_software (OpenH264 forced),
# encode_cpu_convert (BGRA -> NV12 on the CPU forced), encode_warp (convert + encode on Microsoft's
# software adapter, as without a GPU).
param(
    [string]$Bin = $PSScriptRoot,
    [string]$Out = (Join-Path $PSScriptRoot 'probe_out'),
    [string[]]$Only = @()
)
$ErrorActionPreference = 'Continue'
$session = Join-Path $PSScriptRoot 'run_in_session.ps1'
$runs = @(
    @{ Name = 'capture'; Exe = 'capture_probe.exe'; Extra = '5'; Report = 'capture_probe.txt' },
    @{ Name = 'encode_auto'; Exe = 'encode_probe.exe'; Extra = ''; Report = 'encode_probe.txt' },
    @{ Name = 'encode_software'; Exe = 'encode_probe.exe'; Extra = 'force=encode:software'; Report = 'encode_probe.txt' },
    @{ Name = 'encode_cpu_convert'; Exe = 'encode_probe.exe'; Extra = 'force=convert:cpu'; Report = 'encode_probe.txt' },
    @{ Name = 'encode_warp'; Exe = 'encode_probe.exe'; Extra = 'force=adapter:warp'; Report = 'encode_probe.txt' }
)
foreach ($run in $runs) {
    if ($Only.Count -gt 0 -and $run.Name -notin $Only) { continue }
    $dir = Join-Path $Out $run.Name
    if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
    $arguments = ('"{0}" {1}' -f $dir, $run.Extra).Trim()
    "=== $($run.Name)"
    & $session -Exe (Join-Path $Bin $run.Exe) -Arguments $arguments -TimeoutSec 120
    $report = Join-Path $dir $run.Report
    # The probes write UTF-8 (Windows error texts can be Chinese); PS 5.1 would read ANSI by default.
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report -Encoding UTF8 } else { "no report at $report" }
}
