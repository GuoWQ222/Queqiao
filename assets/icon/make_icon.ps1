# Makes the icons from the magpies drawn by magpie_icon.js: assets\queqiao.ico (the program's icon, every size Windows
# asks for) and assets\magpie_spread.ico (the magpie with its wings spread, for the main window's card). Renders each
# SVG at each size with Microsoft Edge (headless, in a profile of its own in %TEMP%), cuts each size out with its
# transparency and packs them as PNG images into the .ico. Needs Node.js and Microsoft Edge. Run it after changing
# the drawing:
#   powershell -NoProfile -ExecutionPolicy Bypass -File assets\icon\make_icon.ps1
# Prints ICON_DONE <file> per icon, or ICON_FAIL <reason>. ASCII only.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$here = $PSScriptRoot
$assets = Split-Path -Parent $here
$work = Join-Path $env:TEMP 'queqiao_icon_work'
$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $edge) { 'ICON_FAIL Microsoft Edge not found'; return }
if (Test-Path -LiteralPath $work) { Remove-Item -Recurse -Force -LiteralPath $work }
New-Item -ItemType Directory -Force $work | Out-Null

& node (Join-Path $here 'magpie_icon.js') $work | Out-Null
if ($LASTEXITCODE) { 'ICON_FAIL magpie_icon.js'; return }

# One .ico: each (size, svg) side by side on one transparent page, a screenshot, each size cut out. The window is at
# least 640 x 320: headless Edge did not finish with a small one (248 x 64, 2026-11-23).
function New-Icon([object[]]$Images, [string]$Target, [string]$Name) {
    $x = 0
    $places = @()
    $tags = foreach ($image in $Images) {
        $size, $file = $image
        $places += , @($size, $x)
        "<img src=`"$file`" style=`"position:absolute;left:${x}px;top:0;width:${size}px;height:${size}px`">"
        $x += $size + 8
    }
    $height = ($Images | ForEach-Object { $_[0] } | Measure-Object -Maximum).Maximum
    $page = "<!doctype html><meta charset=`"utf-8`"><style>html,body{margin:0;background:transparent}</style>" + ($tags -join '')
    $html = Join-Path $work "$Name.html"
    Set-Content -LiteralPath $html -Value $page -Encoding ascii
    $shot = Join-Path $work "$Name.png"
    $arguments = @('--headless=new', '--disable-gpu', '--hide-scrollbars', '--no-first-run', '--no-default-browser-check',
        "--user-data-dir=$(Join-Path $work "profile-$Name")", '--force-device-scale-factor=1', '--default-background-color=00000000',
        "--window-size=$([Math]::Max($x, 640)),$([Math]::Max($height, 320))", "--screenshot=$shot", ([Uri]$html).AbsoluteUri)
    $process = Start-Process -FilePath $edge -ArgumentList $arguments -PassThru -WindowStyle Hidden
    if (-not $process.WaitForExit(60000)) { Stop-Process -Id $process.Id -Force; "ICON_FAIL $Name`: Edge did not finish"; return }
    if (-not (Test-Path -LiteralPath $shot)) { "ICON_FAIL $Name`: no screenshot"; return }
    $sheet = [System.Drawing.Bitmap]::FromFile($shot)
    # A blank or opaque sheet means the page did not render as it should.
    $corner = $sheet.GetPixel($x - 2, $height - 2)
    $first = $Images[0][0]
    $middle = $sheet.GetPixel([int]($first / 2), [int]($first / 2))
    if ($corner.A -ne 0) { $sheet.Dispose(); "ICON_FAIL $Name`: the background is not transparent"; return }
    if ($middle.A -eq 0) { $sheet.Dispose(); "ICON_FAIL $Name`: the drawing is missing"; return }
    $pngs = foreach ($place in $places) {
        $size, $left = $place
        $piece = $sheet.Clone((New-Object System.Drawing.Rectangle $left, 0, $size, $size), [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $stream = New-Object System.IO.MemoryStream
        $piece.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        $piece.Dispose()
        , @($size, $stream.ToArray())
    }
    $sheet.Dispose()
    # The .ico: header, one directory entry per image, then the PNG data.
    $file = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter $file
    $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$pngs.Count)
    $offset = 6 + 16 * $pngs.Count
    foreach ($png in $pngs) {
        $size, $bytes = $png
        $edgeByte = if ($size -ge 256) { 0 } else { $size }
        $w.Write([byte]$edgeByte); $w.Write([byte]$edgeByte); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$bytes.Length); $w.Write([uint32]$offset)
        $offset += $bytes.Length
    }
    foreach ($png in $pngs) { $w.Write([byte[]]$png[1]) }
    $w.Flush()
    [IO.File]::WriteAllBytes($Target, $file.ToArray())
    "ICON_DONE $Target $((Get-Item -LiteralPath $Target).Length) bytes, sizes $(($Images | ForEach-Object { $_[0] }) -join ', ')"
}

# The program's icon: the small sizes from the drawing with the stronger rim.
$app = foreach ($size in 256, 128, 96, 64, 48, 40, 32, 24, 20, 16) {
    , @($size, $(if ($size -le 24) { 'queqiao-small.svg' } else { 'queqiao.svg' }))
}
New-Icon $app (Join-Path $assets 'queqiao.ico') 'app'
# The card's magpie.
$spread = foreach ($size in 64, 48, 40, 32, 24) { , @($size, 'magpie-spread.svg') }
New-Icon $spread (Join-Path $assets 'magpie_spread.ico') 'spread'
Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
