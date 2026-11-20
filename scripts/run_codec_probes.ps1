# Runs the H.264 round-trip probe (codec_probe.exe) for every encoder/decoder combination and prints the
# reports. It captures nothing, so it runs anywhere, also over SSH. Copy this script and codec_probe.exe
# (release build) into one folder on a test machine, or point -Bin at target\release\examples.
# Usage: scripts\run_codec_probes.ps1 [-Bin <folder with codec_probe.exe>] [-Size 1920x1080] [-Frames 120]
param(
    [string]$Bin = $PSScriptRoot,
    [string]$Size = '1920x1080',
    [int]$Frames = 120
)
$ErrorActionPreference = 'Continue'
$width, $height = $Size -split 'x'
$exe = Join-Path $Bin 'codec_probe.exe'
$runs = [ordered]@{
    'GPU encode, GPU decode' = ''
    'GPU encode, OpenH264 decode' = 'force=decode:software'
    'OpenH264 encode, GPU decode' = 'force=encode:software'
    'OpenH264 encode and decode' = 'force=encode:software,decode:software'
    'no GPU (WARP)' = 'force=adapter:warp'
}
foreach ($name in $runs.Keys) {
    "=== $name"
    $arguments = @("width=$width", "height=$height", "frames=$Frames")
    if ($runs[$name]) { $arguments += $runs[$name] }
    & $exe @arguments 2>&1 | ForEach-Object { "$_" }
}
