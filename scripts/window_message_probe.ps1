# Starts message_probe.exe (an example of queqiao-display) at Medium integrity, the level of a person's ordinary
# programs: it sends the agent's tray window two messages and writes what Windows let through to -Report.
# (PowerShell itself cannot be the Medium side: Add-Type fails there.)
# Usage: window_message_probe.ps1 -Probe <message_probe.exe> -AgentId <id> -Report <file>
param([string]$Probe, [int]$AgentId, [string]$Report)
Add-Type -Namespace QueqiaoWindows -Name Native -MemberDefinition @"
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetCurrentProcess();
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(System.IntPtr handle, uint milliseconds);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern bool OpenProcessToken(System.IntPtr process, uint access, out System.IntPtr token);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern bool DuplicateTokenEx(System.IntPtr token, uint access, System.IntPtr attributes, int level, int type, out System.IntPtr copy);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public static extern bool ConvertStringSidToSid(string sid, out System.IntPtr psid);
[System.Runtime.InteropServices.DllImport("advapi32.dll")] public static extern int GetLengthSid(System.IntPtr sid);
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)] public struct Label { public System.IntPtr Sid; public uint Attributes; }
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern bool SetTokenInformation(System.IntPtr token, int kind, ref Label label, uint length);
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public struct StartupInfo { public int cb; public string reserved; public string desktop; public string title; public int x; public int y; public int xSize; public int ySize; public int xChars; public int yChars; public int fill; public int flags; public short show; public short reserved2; public System.IntPtr reserved3; public System.IntPtr stdIn; public System.IntPtr stdOut; public System.IntPtr stdErr; }
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)] public struct ProcessInfo { public System.IntPtr process; public System.IntPtr thread; public int processId; public int threadId; }
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public static extern bool CreateProcessAsUser(System.IntPtr token, string application, string command, System.IntPtr processAttributes, System.IntPtr threadAttributes, bool inherit, uint flags, System.IntPtr environment, string folder, ref StartupInfo startup, out ProcessInfo information);
"@
$native = [QueqiaoWindows.Native]
# A copy of this program's token at Medium (lowering needs no rights). TOKEN_DUPLICATE | TOKEN_QUERY |
# TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT; the copy: TOKEN_ALL_ACCESS, SecurityImpersonation, TokenPrimary.
$token = [IntPtr]::Zero
$copy = [IntPtr]::Zero
$sid = [IntPtr]::Zero
$ok = $native::OpenProcessToken($native::GetCurrentProcess(), 0x8B, [ref]$token) -and $native::DuplicateTokenEx($token, 0xF01FF, [IntPtr]::Zero, 2, 1, [ref]$copy) -and $native::ConvertStringSidToSid('S-1-16-8192', [ref]$sid)
if (-not $ok) { Set-Content -LiteralPath $Report -Value "no Medium token ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"; exit 1 }
$label = New-Object QueqiaoWindows.Native+Label
$label.Sid = $sid
# SE_GROUP_INTEGRITY; TokenIntegrityLevel = 25.
$label.Attributes = 0x20
$length = [uint32]([Runtime.InteropServices.Marshal]::SizeOf($label) + $native::GetLengthSid($sid))
if (-not $native::SetTokenInformation($copy, 25, [ref]$label, $length)) { Set-Content -LiteralPath $Report -Value "level not set ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"; exit 1 }
$startup = New-Object QueqiaoWindows.Native+StartupInfo
$startup.cb = [Runtime.InteropServices.Marshal]::SizeOf($startup)
# The person's desktop, where the agent's windows are.
$startup.desktop = 'winsta0\default'
$information = New-Object QueqiaoWindows.Native+ProcessInfo
$command = "`"$Probe`" $AgentId `"$Report`""
# CREATE_NO_WINDOW. A $null string would reach Windows as "" (an invalid name): no program name, the probe's folder.
if (-not $native::CreateProcessAsUser($copy, [NullString]::Value, $command, [IntPtr]::Zero, [IntPtr]::Zero, $false, 0x08000000, [IntPtr]::Zero, (Split-Path -Parent $Probe), [ref]$startup, [ref]$information)) {
    Set-Content -LiteralPath $Report -Value "the Medium probe did not start ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
    exit 1
}
[void]$native::WaitForSingleObject($information.process, 30000)
