# M5 check of the sign-in screen, on a TEST PC (run as an administrator; the console session is disconnected for the
# test and connected again after, without a password: it is the same user's; WTSDisconnectSession and
# WTSConnectSession, which Windows Home has too, unlike tsdiscon and tscon). queqiao-server (fresh data folder, one
# user), queqiao-service installed with that test account, then
# 1. the person's agent hands this device to the service ("kept for it" in the service log);
# 2. the person's session is disconnected: the console shows the sign-in screen with nobody signed in
#    there, and the service starts the desktop helper and the agent for the sign-in screen (LocalService, session 0);
# 3. a viewer in the person's (disconnected) session connects: pictures of the sign-in screen through the helper, the
#    banner shown there, Ctrl+Alt+Del and Escape injected by the helper;
# 4. the person's session goes back to the console and the service starts the person's agent again.
# The service is removed at the end (-Keep leaves it). Prints M5_LOGON_PASS or M5_LOGON_FAIL. ASCII only.
# Usage: scripts\run_m5_logon.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 20] [-Keep]
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m5_logon'),
    [int]$Seconds = 20,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item -Path "$Out\*.txt" -ErrorAction SilentlyContinue
$logonOut = Join-Path $Out 'logon'
New-Item -ItemType Directory -Force $logonOut | Out-Null
Remove-Item -Path "$logonOut\*" -ErrorAction SilentlyContinue
# The agent for the sign-in screen runs as LocalService: it may write its report here only.
& icacls.exe $logonOut /grant '*S-1-5-19:(OI)(CI)M' | Out-Null
$data = Join-Path $Out 'server-data'
if (Test-Path -LiteralPath $data) { Remove-Item -Recurse -Force -LiteralPath $data }

function Start-SessionTask([string]$Name, [string]$Exe, [string]$Arguments) {
    $action = New-ScheduledTaskAction -Execute $Exe -Argument $Arguments -WorkingDirectory (Split-Path -Parent $Exe)
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds ($Seconds + 180)) -Priority 4
    Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $Name
}
function Wait-SessionTask([string]$Name, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        Start-Sleep -Milliseconds 300
        $result = (Get-ScheduledTaskInfo -TaskName $Name).LastTaskResult
        $state = (Get-ScheduledTask -TaskName $Name).State
        $busy = ($state -eq 'Running') -or ($result -eq 267009) -or ($result -eq 267011)
    } while ($busy -and (Get-Date) -lt $deadline)
    if ($busy) { Stop-ScheduledTask -TaskName $Name; $result = 'timeout' }
    Unregister-ScheduledTask -TaskName $Name -Confirm:$false
    $result
}
$serviceLog = Join-Path $env:ProgramData 'Queqiao\service.log'
$logSkip = if (Test-Path -LiteralPath $serviceLog) { @(Get-Content -LiteralPath $serviceLog).Count } else { 0 }
function Get-ServiceLog { if (Test-Path -LiteralPath $serviceLog) { @(Get-Content -LiteralPath $serviceLog | Select-Object -Skip $logSkip) } else { @() } }
function Wait-ServiceLog([string]$Pattern, [int]$Count, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        if (@(Get-ServiceLog | Where-Object { $_ -match $Pattern }).Count -ge $Count) { return $true }
        Start-Sleep -Milliseconds 300
    } while ((Get-Date) -lt $deadline)
    $false
}
Add-Type -Namespace QueqiaoTest -Name Wts -MemberDefinition @"
[System.Runtime.InteropServices.DllImport("wtsapi32.dll", SetLastError = true)] public static extern bool WTSDisconnectSession(System.IntPtr server, int session, bool wait);
[System.Runtime.InteropServices.DllImport("wtsapi32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)] public static extern bool WTSConnectSessionW(int target, int source, string password, bool wait);
[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern int WTSGetActiveConsoleSessionId();
"@
function Disconnect-Session([int]$Session) {
    $result = [QueqiaoTest.Wts]::WTSDisconnectSession([IntPtr]::Zero, $Session, $true)
    "disconnected session " + $Session + ": $result ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
}
# The person's session back on the console (no password: the same user).
function Connect-Session([int]$Session) {
    $console = [QueqiaoTest.Wts]::WTSGetActiveConsoleSessionId()
    $result = [QueqiaoTest.Wts]::WTSConnectSessionW($Session, $console, '', $true)
    "connected session $Session to the console (was $console): $result ($([Runtime.InteropServices.Marshal]::GetLastWin32Error()))"
}
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-56} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}
function Get-Number([string[]]$Text, [string]$Pattern) {
    $match = [regex]::Match(($Text -join "`n"), $Pattern)
    if ($match.Success) { [int]$match.Groups[1].Value } else { -1 }
}

$server = Join-Path $Bin 'queqiao-server.exe'
$viewer = Join-Path $Bin 'queqiao-viewer.exe'
$serviceExe = Join-Path $Bin 'queqiao-service.exe'
$listen = '127.0.0.1:47100'
$device = $env:COMPUTERNAME
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "RUN_FAIL no fingerprint from: $init"; return }
$password = [guid]::NewGuid().ToString('N')
& $server add-user alice $password --data $data | Out-Null
$serveArgs = @('serve', '--listen', $listen, '--data', "`"$data`"", '--public-ip', '127.0.0.1', '--relay-private')
$serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
# The session at the screen: the one Explorer runs in.
$personSession = [int](Get-CimInstance Win32_Process -Filter "Name = 'explorer.exe'" | Select-Object -First 1).SessionId
"the person's session: $personSession"
$failed = @()
$completed = $false
$disconnected = $false
try {
    Start-Sleep -Seconds 1
    '=== 1. install: the person''s agent hands the device to the service'
    $agentArgs = "--server $listen --pin $pin --user alice --password $password --sessions 3 --seconds 400 --dry-run-commands --report `"$Out\agent_person.txt`""
    $logonArgs = "--sessions 1 --seconds 150 --dry-run-commands --report `"$logonOut\agent_logon.txt`""
    & { $ErrorActionPreference = 'Continue'; & $serviceExe install --agent-arguments $agentArgs --logon-arguments $logonArgs --no-restart 2>&1 | ForEach-Object { "$_" } }
    Add-Check 'service: the person''s agent started' (Wait-ServiceLog 'started the desktop helper \(\d+\) and the agent' 1 30)
    Add-Check 'service: the device kept for the sign-in screen' (Wait-ServiceLog 'the sign-in screen: device \S+ kept for it' 1 40)
    $file = Join-Path $env:ProgramData 'Queqiao\logon.bin'
    $access = if (Test-Path -LiteralPath $file) { (Get-Acl -LiteralPath $file).Access | ForEach-Object { "$($_.IdentityReference):$($_.FileSystemRights)" } } else { @() }
    "logon.bin access: $($access -join '; ')"
    Add-Check 'logon.bin: SYSTEM only' (($access.Count -eq 1) -and ($access[0] -like '*SYSTEM*'))

    '=== 2. the person''s session disconnected: nobody signed in at the screen'
    Disconnect-Session $personSession
    $disconnected = $true
    Add-Check 'service: the agent for the sign-in screen started' (Wait-ServiceLog 'started the desktop helper \(\d+\) and the agent for the sign-in screen' 1 40)
    $logonAgent = Get-CimInstance Win32_Process -Filter "Name = 'queqiao-agent.exe'" | Where-Object { $_.CommandLine -match ' --logon ' } | Select-Object -First 1
    if ($logonAgent) {
        $owner = Invoke-CimMethod -InputObject $logonAgent -MethodName GetOwner
        "agent for the sign-in screen: process $($logonAgent.ProcessId), session $($logonAgent.SessionId), $($owner.Domain)\$($owner.User)"
        Add-Check 'logon agent: LocalService in session 0' (($logonAgent.SessionId -eq 0) -and ($owner.User -match 'LOCAL SERVICE'))
    } else {
        Add-Check 'logon agent: LocalService in session 0' $false
    }

    '=== 3. a viewer connects to the sign-in screen'
    Start-Sleep -Seconds 4
    Start-SessionTask 'Queqiao-M5-Logon-Viewer' $viewer "--server $listen --pin $pin --user alice --password $password --device $device --seconds $Seconds --secure-test --width 960 --report `"$Out\viewer_logon.txt`""
    $viewerResult = Wait-SessionTask 'Queqiao-M5-Logon-Viewer' ($Seconds + 150)
    "viewer task: $viewerResult"
    $deadline = (Get-Date).AddSeconds(40)
    while (-not (Test-Path -LiteralPath "$logonOut\agent_logon.txt") -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    $viewerText = if (Test-Path -LiteralPath "$Out\viewer_logon.txt") { @(Get-Content -LiteralPath "$Out\viewer_logon.txt" -Encoding UTF8) } else { @('no viewer report') }
    $agentText = if (Test-Path -LiteralPath "$logonOut\agent_logon.txt") { @(Get-Content -LiteralPath "$logonOut\agent_logon.txt" -Encoding UTF8) } else { @('no agent report') }
    '--- viewer'
    $viewerText
    '--- agent for the sign-in screen'
    $agentText
    Add-Check 'logon agent: given this device' (($agentText -match '^the agent for the sign-in screen, as').Count -gt 0)
    Add-Check 'logon agent: a session' (($agentText -match '^session 1: viewer').Count -gt 0)
    Add-Check 'logon agent: pictures from the helper' ((Get-Number $agentText '(\d+) pictures from it') -ge 1)
    Add-Check 'logon agent: input passed to the helper' ((Get-Number $agentText '(\d+) inputs passed to it') -ge 2)
    Add-Check 'viewer: pictures of the sign-in screen' ((Get-Number $viewerText '(\d+) frames received') -ge 1)
    Add-Check 'helper: banner shown on the sign-in screen' (@(Get-ServiceLog | Where-Object { $_ -match 'banner shown on Winlogon' }).Count -gt 0)
    [void](Wait-ServiceLog 'desktop helper: ending .*injected \d+' 1 20)
    Add-Check 'helper: input injected there' ((Get-Number (Get-ServiceLog) 'injected (\d+)') -ge 2)

    '=== 4. the person''s session back at the screen: their agent starts again'
    $startsBefore = @(Get-ServiceLog | Where-Object { $_ -match 'started the desktop helper \(\d+\) and the agent \(' }).Count
    Connect-Session $personSession
    $disconnected = [QueqiaoTest.Wts]::WTSGetActiveConsoleSessionId() -ne $personSession
    Add-Check 'service: the person''s agent started again' (Wait-ServiceLog 'started the desktop helper \(\d+\) and the agent \(' ($startsBefore + 1) 40)
    $completed = $true
} finally {
    if ($disconnected) { Connect-Session $personSession }
    "console session now: $([QueqiaoTest.Wts]::WTSGetActiveConsoleSessionId()) (the person's: $personSession)"
    '=== service log (this run)'
    Get-ServiceLog
    if (-not $Keep) { & { $ErrorActionPreference = 'Continue'; & $serviceExe uninstall 2>&1 | ForEach-Object { "$_" } } }
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    if (-not $completed -and -not $failed.Count) { $failed += 'stopped early' }
    if ($failed.Count) { "M5_LOGON_FAIL $($failed -join ', ')" } else { 'M5_LOGON_PASS' }
}
