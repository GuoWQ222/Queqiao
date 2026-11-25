# Tests: a File Explorer window of its own (C:\Windows\System32, many icons), moved round the screen about 60 times a
# second for -Seconds after -Delay, as when a person drags it; then closed. The person's own windows are not touched.
# Runs in the desktop session (scripts\run_m4_local.ps1 starts it as a session task). Prints DRAG_DONE <moves> or
# DRAG_FAIL <reason>. ASCII only.
# -Period: seconds for one circle (4: an ordinary drag; under 1: flung about as fast as a hand can).
param([double]$Delay = 0, [double]$Seconds = 10, [double]$Period = 4)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class DragWin {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc proc, IntPtr param);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr hwnd, StringBuilder name, int length);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
    public static List<IntPtr> Explorers() {
        var found = new List<IntPtr>();
        EnumWindows((hwnd, param) => {
            var name = new StringBuilder(64);
            GetClassNameW(hwnd, name, 64);
            if (name.ToString() == "CabinetWClass" && IsWindowVisible(hwnd)) { found.Add(hwnd); }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
$before = [DragWin]::Explorers()
Start-Process -FilePath explorer.exe -ArgumentList 'C:\Windows\System32'
$window = [IntPtr]::Zero
$deadline = (Get-Date).AddSeconds(15)
while ($window -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 200
    $new = @([DragWin]::Explorers() | Where-Object { $before -notcontains $_ })
    if ($new.Count) { $window = $new[0] }
}
if ($window -eq [IntPtr]::Zero) { 'DRAG_FAIL no new File Explorer window'; return }
$screenWidth = [DragWin]::GetSystemMetrics(0)
$screenHeight = [DragWin]::GetSystemMetrics(1)
$width = [int]($screenWidth * 0.45)
$height = [int]($screenHeight * 0.5)
# SWP_NOACTIVATE 0x10, SWP_SHOWWINDOW 0x40: on top, where the person would have dragged it.
[DragWin]::SetWindowPos($window, [IntPtr]::Zero, [int]($screenWidth * 0.1), [int]($screenHeight * 0.1), $width, $height, 0x40) | Out-Null
Start-Sleep -Milliseconds ([int]($Delay * 1000))
$moves = 0
$started = Get-Date
while (((Get-Date) - $started).TotalSeconds -lt $Seconds) {
    $t = ((Get-Date) - $started).TotalSeconds
    # A circle across most of the screen, once every -Period seconds.
    $x = [int](($screenWidth - $width) / 2 + ($screenWidth - $width) * 0.4 * [Math]::Cos($t * 2 * [Math]::PI / $Period))
    $y = [int](($screenHeight - $height) / 2 + ($screenHeight - $height) * 0.4 * [Math]::Sin($t * 2 * [Math]::PI / $Period))
    [DragWin]::SetWindowPos($window, [IntPtr]::Zero, $x, $y, $width, $height, 0x10) | Out-Null
    $moves++
    Start-Sleep -Milliseconds 16
}
# WM_CLOSE
[DragWin]::PostMessageW($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
"DRAG_DONE $moves moves in $([Math]::Round(((Get-Date) - $started).TotalSeconds, 1)) s"
