# Makes the phone apps' pictures from assets\queqiao.ico (the magpie, as the desktop program has it). Android:
#   app\src\main\res\mipmap-<density>\ic_launcher.png  the launcher icon: the magpie on a night-blue rounded square
#   app\src\main\res\drawable-nodpi\magpie.png          the magpie alone (the screens' headers, the session's ball)
# iOS (mobile\ios\Queqiao\Assets.xcassets): AppIcon.appiconset\icon.png (1024 px, opaque: iOS rounds the corners) and
# Magpie.imageset\magpie.png.
# Run it after changing the icon:
#   powershell -NoProfile -ExecutionPolicy Bypass -File mobile\android\make_icons.ps1
# Prints ICONS_DONE, or ICONS_FAIL <reason>. ASCII only.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$res = Join-Path $PSScriptRoot 'app\src\main\res'
$bytes = [IO.File]::ReadAllBytes((Join-Path $repo 'assets\queqiao.ico'))

# The largest image in the .ico (each entry: width, height, ..., size at 8, offset at 12; 0 = 256).
$count = [BitConverter]::ToUInt16($bytes, 4)
$best = $null
for ($i = 0; $i -lt $count; $i++) {
    $at = 6 + 16 * $i
    $w = [int]$bytes[$at]
    if ($w -eq 0) { $w = 256 }
    if (-not $best -or $w -gt $best.Width) {
        $best = @{ Width = $w; Size = [BitConverter]::ToInt32($bytes, $at + 8); Offset = [BitConverter]::ToInt32($bytes, $at + 12) }
    }
}
if (-not $best) { 'ICONS_FAIL no image in queqiao.ico'; return }
$stream = New-Object IO.MemoryStream(, $bytes[$best.Offset..($best.Offset + $best.Size - 1)])
$magpie = [Drawing.Image]::FromStream($stream)

function Save-Picture([int]$size, [string]$path, [bool]$square, [bool]$full = $false) {
    $format = if ($full) { [Drawing.Imaging.PixelFormat]::Format24bppRgb } else { [Drawing.Imaging.PixelFormat]::Format32bppArgb }
    $bitmap = New-Object Drawing.Bitmap($size, $size, $format)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([Drawing.Color]::Transparent)
    $inset = 0
    if ($full) {
        # Full bleed, no transparency (an iOS app icon).
        $brush = New-Object Drawing.Drawing2D.LinearGradientBrush((New-Object Drawing.Point(0, 0)), (New-Object Drawing.Point(0, $size)),
            [Drawing.Color]::FromArgb(255, 0x25, 0x30, 0x70), [Drawing.Color]::FromArgb(255, 0x14, 0x1A, 0x3B))
        $g.FillRectangle($brush, 0, 0, $size, $size)
        $inset = [int]($size * 0.16)
    } elseif ($square) {
        # The rounded square of a launcher icon (Android's legacy icons: about 92% of the size, corners 18%).
        $edge = [int]($size * 0.04)
        $side = $size - 2 * $edge
        $r = [int]($side * 0.18)
        $path2 = New-Object Drawing.Drawing2D.GraphicsPath
        $path2.AddArc($edge, $edge, 2 * $r, 2 * $r, 180, 90)
        $path2.AddArc($edge + $side - 2 * $r, $edge, 2 * $r, 2 * $r, 270, 90)
        $path2.AddArc($edge + $side - 2 * $r, $edge + $side - 2 * $r, 2 * $r, 2 * $r, 0, 90)
        $path2.AddArc($edge, $edge + $side - 2 * $r, 2 * $r, 2 * $r, 90, 90)
        $path2.CloseFigure()
        $brush = New-Object Drawing.Drawing2D.LinearGradientBrush((New-Object Drawing.Point(0, 0)), (New-Object Drawing.Point(0, $size)),
            [Drawing.Color]::FromArgb(255, 0x25, 0x30, 0x70), [Drawing.Color]::FromArgb(255, 0x14, 0x1A, 0x3B))
        $g.FillPath($brush, $path2)
        $inset = [int]($size * 0.17)
    }
    $g.DrawImage($magpie, $inset, $inset, $size - 2 * $inset, $size - 2 * $inset)
    $g.Dispose()
    New-Item -ItemType Directory -Force (Split-Path -Parent $path) | Out-Null
    $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
}

foreach ($density in @(@('mdpi', 48), @('hdpi', 72), @('xhdpi', 96), @('xxhdpi', 144), @('xxxhdpi', 192))) {
    Save-Picture $density[1] (Join-Path $res "mipmap-$($density[0])\ic_launcher.png") $true
}
Save-Picture 192 (Join-Path $res 'drawable-nodpi\magpie.png') $false
$assets = Join-Path (Split-Path -Parent $PSScriptRoot) 'ios\Queqiao\Assets.xcassets'
Save-Picture 1024 (Join-Path $assets 'AppIcon.appiconset\icon.png') $false $true
Save-Picture 192 (Join-Path $assets 'Magpie.imageset\magpie.png') $false
$magpie.Dispose()
'ICONS_DONE'
