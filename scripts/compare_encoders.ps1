# Compares H.264 encoder paths on this machine with the release probes: the round trip (quality, data rate)
# at two sizes, live encoding of the captured screen (time per frame), and the whole local loop (delay).
# Usage: scripts\compare_encoders.ps1 [-Bin target\release\examples] [-Paths mf,nvenc] [-Out <folder>]
# Paths: mf (Media Foundation hardware), nvenc (NVIDIA direct), software (OpenH264).
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\examples'),
    [string[]]$Paths = @('mf', 'nvenc'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\compare')
)
$ErrorActionPreference = 'Continue'
$session = Join-Path $PSScriptRoot 'run_in_session.ps1'
$keep = 'encoder:|decoder:|frames:|first frame|luma PSNR|decode call|encode time|scroll \+ convert|other frames|stream:|convert \+ encode|screen change -> shown|shown \(|checks:|RESULT|ERROR'
foreach ($path in $Paths) {
    $force = "encode:$path"
    foreach ($size in '1920x1080', '2560x1440') {
        $w, $h = $size -split 'x'
        "=== $path round trip $size"
        & (Join-Path $Bin 'codec_probe.exe') "width=$w" "height=$h" 'frames=300' "force=$force" 2>&1 |
            ForEach-Object { "$_" } | Where-Object { $_ -match $keep }
    }
    foreach ($probe in @(@('encode_probe', 'encode_probe.txt', ''), @('loopback', 'loopback.txt', 'seconds=8'))) {
        $dir = Join-Path $Out "$($probe[0])_$path"
        if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
        "=== $path $($probe[0]) (live screen, desktop session)"
        $arguments = ('"{0}" force={1} {2}' -f $dir, $force, $probe[2]).Trim()
        & $session -Exe (Join-Path $Bin "$($probe[0]).exe") -Arguments $arguments -TimeoutSec 120 | Out-Null
        $report = Join-Path $dir $probe[1]
        if (Test-Path $report) { Get-Content $report -Encoding UTF8 | Where-Object { $_ -match $keep } } else { 'no report' }
    }
}
