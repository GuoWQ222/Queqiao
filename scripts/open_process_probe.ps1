# Opens a process with several access rights and reports what was granted: first as this process is, then after
# removing SeDebugPrivilege from its own token (an administrator's elevated programs have it enabled and may open any
# process; a standard user's programs, and an administrator's when UAC filters the token, do not have it).
# Also reports the process's security descriptor (SDDL).
# Usage: open_process_probe.ps1 -ProcessId <id> -Report <file>
param([int]$ProcessId, [string]$Report)
Add-Type -Namespace QueqiaoProbe -Name Native -MemberDefinition @"
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)] public static extern System.IntPtr OpenProcess(uint access, bool inherit, uint id);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern bool CloseHandle(System.IntPtr handle);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetCurrentProcess();
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern bool OpenProcessToken(System.IntPtr process, uint access, out System.IntPtr token);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public static extern bool LookupPrivilegeValue(string system, string name, out long luid);
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential, Pack = 4)] public struct Privilege { public uint Count; public long Luid; public uint Attributes; }
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern bool AdjustTokenPrivileges(System.IntPtr token, bool disableAll, ref Privilege state, uint length, System.IntPtr previous, System.IntPtr returned);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true)] public static extern uint GetSecurityInfo(System.IntPtr handle, int type, uint info, out System.IntPtr owner, out System.IntPtr group, out System.IntPtr dacl, out System.IntPtr sacl, out System.IntPtr descriptor);
[System.Runtime.InteropServices.DllImport("advapi32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public static extern bool ConvertSecurityDescriptorToStringSecurityDescriptor(System.IntPtr descriptor, uint revision, uint info, out System.IntPtr text, out uint length);
"@
$rights = @(@('copy-handles', 0x40), @('write-memory', 0x28), @('start-thread', 0x2), @('full', 0x1FFFFF), @('wait-and-end', 0x101001))
function Test-Rights([string]$Phase) {
    foreach ($right in $rights) {
        $opened = [QueqiaoProbe.Native]::OpenProcess([uint32]$right[1], $false, [uint32]$ProcessId)
        $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        if ($opened -ne [IntPtr]::Zero) { [void][QueqiaoProbe.Native]::CloseHandle($opened); "$Phase $($right[0]) opened" } else { "$Phase $($right[0]) refused $code" }
    }
}
$lines = @()
# The raw line (its state word is in Windows' language): this file stays ASCII, as PowerShell 5.1 reads a file
# without a byte-order mark in the system code page.
$debug = whoami /priv | Select-String -Pattern 'SeDebugPrivilege' | ForEach-Object { $_.Line -replace '\s+', ' ' }
$lines += 'debug privilege: ' + $(if ($debug) { $debug } else { 'absent' })
$handle = [QueqiaoProbe.Native]::OpenProcess(0x00020000, $false, [uint32]$ProcessId)
if ($handle -ne [IntPtr]::Zero) {
    $owner = [IntPtr]::Zero; $group = [IntPtr]::Zero; $dacl = [IntPtr]::Zero; $sacl = [IntPtr]::Zero; $descriptor = [IntPtr]::Zero
    if ([QueqiaoProbe.Native]::GetSecurityInfo($handle, 6, 0x5, [ref]$owner, [ref]$group, [ref]$dacl, [ref]$sacl, [ref]$descriptor) -eq 0) {
        $text = [IntPtr]::Zero; $length = 0
        [void][QueqiaoProbe.Native]::ConvertSecurityDescriptorToStringSecurityDescriptor($descriptor, 1, 0x5, [ref]$text, [ref]$length)
        $lines += 'descriptor: ' + [Runtime.InteropServices.Marshal]::PtrToStringUni($text)
    }
    [void][QueqiaoProbe.Native]::CloseHandle($handle)
}
$lines += Test-Rights 'as-is'
# Remove SeDebugPrivilege from this process's token (SE_PRIVILEGE_REMOVED), then try again.
$token = [IntPtr]::Zero
$luid = 0
if ([QueqiaoProbe.Native]::OpenProcessToken([QueqiaoProbe.Native]::GetCurrentProcess(), 0x28, [ref]$token) -and [QueqiaoProbe.Native]::LookupPrivilegeValue($null, 'SeDebugPrivilege', [ref]$luid)) {
    $state = New-Object QueqiaoProbe.Native+Privilege
    $state.Count = 1; $state.Luid = $luid; $state.Attributes = 4
    $removed = [QueqiaoProbe.Native]::AdjustTokenPrivileges($token, $false, [ref]$state, 0, [IntPtr]::Zero, [IntPtr]::Zero)
    $lines += 'debug privilege removed: ' + $removed + ' (' + [Runtime.InteropServices.Marshal]::GetLastWin32Error() + ')'
    [void][QueqiaoProbe.Native]::CloseHandle($token)
}
$lines += Test-Rights 'no-debug'
$lines | Set-Content -LiteralPath $Report -Encoding utf8
