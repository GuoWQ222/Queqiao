# M5 check on one PC (run as an administrator): queqiao-server (fresh data folder, one user), then queqiao-service
# installed with that test account as the agent's arguments, and
# 1. the service starts the desktop helper (SYSTEM) and the agent (as the person at the screen) in the console session;
# 2. the agent's process, as seen by its own user: waiting for it and ending it is allowed; copying its handles, writing
#    into it, starting threads in it or full access are refused;
# 3. a session with the viewer's --secure-test: Ctrl+Alt+Del through the service, the Ctrl+Alt+Del screen shown
#    through the desktop helper, Escape through the helper (which closes that screen), this PC's own desktop again;
# 4. "Restart Queqiao on that PC": the service ends the agent and its helper and starts both again.
# The service is removed at the end (-Keep leaves it installed). Prints M5_SERVICE_PASS or M5_SERVICE_FAIL.
# Usage: scripts\run_m5_service.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 20] [-Keep]
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m5_service'),
    [int]$Seconds = 20,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item -Path "$Out\*.txt" -ErrorAction SilentlyContinue
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
# This run's lines, by their time stamps (installing the service starts its folder, and so the log, anew).
$logSince = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
function Get-ServiceLog { if (Test-Path -LiteralPath $serviceLog) { @(Get-Content -LiteralPath $serviceLog | Where-Object { $_.Length -ge 23 -and $_.Substring(0, 23) -ge $logSince }) } else { @() } }
function Wait-ServiceLog([string]$Pattern, [int]$Count, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        $hits = @(Get-ServiceLog | Where-Object { $_ -match $Pattern }).Count
        if ($hits -ge $Count) { return $true }
        Start-Sleep -Milliseconds 300
    } while ((Get-Date) -lt $deadline)
    $false
}
function Wait-File([string]$Path, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while (-not (Test-Path -LiteralPath $Path) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    Test-Path -LiteralPath $Path
}
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-52} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
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
$password | & $server add-user alice --data $data | Out-Null
$serveArgs = @('serve', '--listen', $listen, '--data', "`"$data`"", '--public-ip', '127.0.0.1', '--relay-private')
$serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
$failed = @()
$completed = $false
try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect('127.0.0.1', 47100); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if (-not $up) { 'RUN_FAIL the server did not start'; return }

    '=== 0. the agent for the sign-in screen (no tray icon, no banner) only as LocalService'
    # Started by a program of a person, --logon is refused; as LocalService (which only the service, or an
    # administrator's task as here, can arrange) it gets past that and stops only for want of the service's channel.
    $agentExe = Join-Path $Bin 'queqiao-agent.exe'
    $probeDir = Join-Path $env:SystemRoot 'Temp\queqiao-logon-probe'
    if (Test-Path -LiteralPath $probeDir) { Remove-Item -Recurse -Force -LiteralPath $probeDir }
    New-Item -ItemType Directory -Force $probeDir | Out-Null
    & icacls.exe $probeDir /grant '*S-1-5-19:(OI)(CI)M' | Out-Null
    $asPerson = @(& $agentExe --logon 2>&1 | ForEach-Object { "$_" }) -join ' '
    Add-Check 'agent --logon refused outside LocalService' ($asPerson -match 'runs as LocalService only')
    # LocalService may not read the build in an administrator's profile: a copy runs from the probe folder.
    Copy-Item -LiteralPath $agentExe -Destination $probeDir
    $action = New-ScheduledTaskAction -Execute (Join-Path $probeDir 'queqiao-agent.exe') -Argument "--logon --report `"$probeDir\local_service.txt`""
    $principal = New-ScheduledTaskPrincipal -UserId 'NT AUTHORITY\LOCALSERVICE' -LogonType ServiceAccount
    Register-ScheduledTask -TaskName 'Queqiao-M5-LogonProbe' -Action $action -Principal $principal -Force | Out-Null
    Start-ScheduledTask -TaskName 'Queqiao-M5-LogonProbe'
    $probeFile = Join-Path $probeDir 'local_service.txt'
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path -LiteralPath $probeFile) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    Start-Sleep -Milliseconds 500
    if (-not (Test-Path -LiteralPath $probeFile)) { "the task's result: $((Get-ScheduledTaskInfo -TaskName 'Queqiao-M5-LogonProbe').LastTaskResult)" }
    Unregister-ScheduledTask -TaskName 'Queqiao-M5-LogonProbe' -Confirm:$false
    $asLocalService = if (Test-Path -LiteralPath $probeFile) { (Get-Content -LiteralPath $probeFile -Encoding UTF8) -join ' ' } else { 'no report' }
    "as LocalService: $(($asLocalService -split 'ERROR ')[-1])"
    Add-Check 'agent --logon as LocalService: past that check' ($asLocalService -match '--logon is for the Queqiao service only' -and $asLocalService -notmatch 'runs as LocalService only')
    Remove-Item -Recurse -Force -LiteralPath $probeDir -ErrorAction SilentlyContinue

    '=== 1. install: the service starts the desktop helper and the agent'
    # Something left in the service's folder before it is installed (as a person may make there) must not survive.
    $dataFolder = Join-Path $env:ProgramData 'Queqiao'
    $planted = Join-Path $dataFolder 'planted-before-install\version.dll'
    New-Item -ItemType Directory -Force (Split-Path $planted) | Out-Null
    Set-Content -LiteralPath $planted -Value 'not Queqiao' -Encoding ascii
    $agentArgs = "--server $listen --pin $pin --user alice --password $password --sessions 1 --seconds 150 --dry-run-commands --test-start-program --report `"$Out\agent_secure.txt`""
    & { $ErrorActionPreference = 'Continue'; & $serviceExe install --agent-arguments $agentArgs --no-restart 2>&1 | ForEach-Object { "$_" } }
    if (-not (Wait-ServiceLog 'started the desktop helper \(\d+\) and the agent \(\d+\)' 1 30)) {
        $failed += 'service start'
        Get-ServiceLog
        return
    }
    Get-ServiceLog | Where-Object { $_ -match 'started|helper' }
    $installed = Join-Path $env:ProgramFiles 'Queqiao'
    $agentProcess = Get-Process queqiao-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$installed\*" } | Select-Object -First 1
    $helperProcess = Get-CimInstance Win32_Process -Filter "Name = 'queqiao-service.exe'" | Where-Object { $_.CommandLine -match ' desktop --channel ' } | Select-Object -First 1
    if (-not $agentProcess -or -not $helperProcess) { $failed += 'processes'; "agent: $($agentProcess.Id) helper: $($helperProcess.ProcessId)"; return }
    $helperOwner = Invoke-CimMethod -InputObject $helperProcess -MethodName GetOwner
    "agent $($agentProcess.Id) in session $($agentProcess.SessionId); helper $($helperProcess.ProcessId) in session $($helperProcess.SessionId) as $($helperOwner.Domain)\$($helperOwner.User)"
    if ($helperOwner.User -ne 'SYSTEM') { $failed += 'helper account' }
    Add-Check 'install: what was in its folder before is gone' (-not (Test-Path -LiteralPath (Split-Path $planted)))
    $dataAcl = Get-Acl -LiteralPath $dataFolder
    Add-Check 'install: its folder has its own access, not inherited' $dataAcl.AreAccessRulesProtected
    Add-Check 'install: users may not write in its folder' (-not @($dataAcl.Access | Where-Object { $_.IdentityReference -match 'Users|Everyone|Authenticated' -and $_.FileSystemRights -match 'Write|Modify|FullControl|CreateFiles|AppendData' }).Count)

    '=== 2. the agent''s process, opened by its own user'
    # An administrator's elevated programs (SeDebugPrivilege enabled) may open any process; the protection is for
    # programs without that privilege, so the probe tries both ways and the check is on the second.
    $probeScript = Join-Path $PSScriptRoot 'open_process_probe.ps1'
    $openReport = Join-Path $Out 'open_agent.txt'
    Start-SessionTask 'Queqiao-M5-Open' (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') "-NoProfile -ExecutionPolicy Bypass -File `"$probeScript`" -ProcessId $($agentProcess.Id) -Report `"$openReport`""
    [void](Wait-SessionTask 'Queqiao-M5-Open' 60)
    # The task can be done before the report is written; Add-Type alone takes seconds.
    [void](Wait-File $openReport 30)
    $opened = @(if (Test-Path -LiteralPath $openReport) { @(Get-Content -LiteralPath $openReport) } else { @('no report') })
    $opened
    foreach ($right in 'copy-handles', 'write-memory', 'start-thread', 'full') {
        Add-Check "protection: $right refused without debug rights" ($opened -contains "no-debug $right refused 5")
    }
    # Waiting for it stays allowed. Ending it too while it runs at the person's own level; at High (raised by the
    # service) a Medium program may not, as with an elevated program (the person ends it from its tray icon).
    Add-Check 'protection: waiting still allowed' ($opened -contains 'no-debug wait opened')

    '=== 2b. the agent''s windows, sent messages by a program at Medium integrity (the person''s ordinary level)'
    # The agent runs at High integrity: a hook or a message from such a program does not reach it, except the tray
    # icon's message (Explorer's clicks).
    $windowScript = Join-Path $PSScriptRoot 'window_message_probe.ps1'
    $windowReport = Join-Path $Out 'window_messages.txt'
    Remove-Item -LiteralPath $windowReport -ErrorAction SilentlyContinue
    Start-SessionTask 'Queqiao-M5-Windows' (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') "-NoProfile -ExecutionPolicy Bypass -File `"$windowScript`" -Probe `"$(Join-Path $Bin 'examples\message_probe.exe')`" -AgentId $($agentProcess.Id) -Report `"$windowReport`""
    [void](Wait-SessionTask 'Queqiao-M5-Windows' 90)
    [void](Wait-File $windowReport 30)
    $messages = @(if (Test-Path -LiteralPath $windowReport) { @(Get-Content -LiteralPath $windowReport) } else { @('no report') })
    $messages
    Add-Check 'windows: the probe ran at Medium integrity' ($messages -contains 'level Medium')
    Add-Check 'windows: its message to the agent refused' ($messages -contains 'other refused 5')
    Add-Check 'windows: the tray icon''s message let through' ($messages -contains 'tray ok')

    '=== 3. a session: Ctrl+Alt+Del, its screen through the desktop helper, Escape, the own desktop again'
    Start-Sleep -Seconds 3
    # --mute: viewer and agent run on this one PC. Played here, the sound the agent captures from this PC's speakers
    # would be captured again and go round (a beep repeated for the whole session, heard on sunshine on 2026-11-22).
    # The agent still makes its sound sender at the start (what the socket check below needs); SoundOff ends it.
    Start-SessionTask 'Queqiao-M5-Viewer' $viewer "--server $listen --pin $pin --user alice --password $password --device $device --seconds $Seconds --secure-test --width 960 --mute --report `"$Out\viewer_secure.txt`""
    $viewerResult = Wait-SessionTask 'Queqiao-M5-Viewer' ($Seconds + 150)
    "viewer task: $viewerResult"
    $agentDone = Wait-File "$Out\agent_secure.txt" 40
    $viewerText = @(if (Test-Path -LiteralPath "$Out\viewer_secure.txt") { @(Get-Content -LiteralPath "$Out\viewer_secure.txt" -Encoding UTF8) } else { @('no viewer report') })
    $agentText = @(if ($agentDone) { @(Get-Content -LiteralPath "$Out\agent_secure.txt" -Encoding UTF8) } else { @('no agent report') })
    # At High integrity; what it starts at Medium when the service raised it (a person whose token is not an
    # administrator's full one), else at its own level.
    $integrity = @($agentText | Where-Object { $_ -match '^integrity: ' } | Select-Object -First 1)
    $started = @($agentText | Where-Object { $_ -match '^test: a program' } | Select-Object -First 1)
    "agent: $integrity; $started"
    Add-Check 'agent: at High integrity' (($integrity.Count -eq 1) -and ($integrity[0] -match '^integrity: High'))
    $startLevel = if (($integrity.Count -eq 1) -and ($integrity[0] -match 'raised by the service')) { 'Medium' } else { 'High' }
    Add-Check "agent: what it starts runs at $startLevel" (($started.Count -eq 1) -and ($started[0] -match "runs at $startLevel integrity"))
    '--- viewer'
    $viewerText
    '--- agent'
    $agentText
    # One call per check: in an array, PowerShell's comma binds before -ge and -and.
    Add-Check 'agent: started by the service' (($agentText -match '^started by the Queqiao service').Count -gt 0)
    # The agent the service starts may not duplicate its own handles (its process is closed to that): sockets are
    # shared, not copied. A copy used to fail with WSAEINVAL and took the sound (and with several uplinks the session).
    Add-Check 'agent: sound starts (no socket copy refused)' (($agentText -match '^sound: not sent').Count -eq 0)
    Add-Check 'agent: Ctrl+Alt+Del through the service' (($agentText -match 'CtrlAltDel: Ctrl\+Alt\+Del sent').Count -gt 0)
    Add-Check 'agent: switched to the desktop helper' (($agentText -match 'another desktop takes input').Count -gt 0)
    Add-Check 'agent: back to its own capture' (($agentText -match 'own desktop takes input again').Count -gt 0)
    Add-Check 'agent: pictures from the helper' ((Get-Number $agentText '(\d+) pictures from it') -ge 1)
    Add-Check 'agent: Escape passed to the helper' ((Get-Number $agentText '(\d+) inputs passed to it') -ge 2)
    Add-Check 'viewer: pictures of the Ctrl+Alt+Del screen' ((Get-Number $viewerText '(\d+) pictures shown while the Ctrl\+Alt\+Del screen was up') -ge 1)
    Add-Check 'viewer: pictures after Escape' ((Get-Number $viewerText '(\d+) pictures shown after Escape') -ge 1)
    Add-Check 'helper: saw the Winlogon desktop' (@(Get-ServiceLog | Where-Object { $_ -match 'input desktop Winlogon' }).Count -gt 0)
    # Of SYSTEM's privileges, the helper keeps SeChangeNotifyPrivilege only (and still does all of the above).
    $helperStarts = @(Get-ServiceLog | Where-Object { $_ -match 'desktop helper: started' })
    "helper: $($helperStarts | Select-Object -Last 1)"
    Add-Check 'helper: SeChangeNotifyPrivilege only' (($helperStarts.Count -gt 0) -and (@($helperStarts | Where-Object { $_ -notmatch 'desktop helper: started \(privileges: SeChangeNotifyPrivilege\)$' }).Count -eq 0))
    # The person's own banner is out of sight there: the helper shows one on that screen.
    Add-Check 'helper: banner shown on the Ctrl+Alt+Del screen' (@(Get-ServiceLog | Where-Object { $_ -match 'banner shown on Winlogon' }).Count -gt 0)
    # The helper's own count, and how soon after Ctrl+Alt+Del the person's desktop was back (Escape goes 2 s after
    # it; without Escape that screen closed by itself after about 4.8 s on this PC).
    [void](Wait-ServiceLog 'desktop helper: ending .*injected \d+' 1 15)
    $injected = Get-Number (Get-ServiceLog) 'injected (\d+)'
    $stamp = { param($line) [datetime]::ParseExact($line.Substring(0, 23), 'yyyy-MM-dd HH:mm:ss.fff', $null) }
    $sasLine = Get-ServiceLog | Where-Object { $_ -match 'Ctrl\+Alt\+Del sent' } | Select-Object -First 1
    $backLine = if ($sasLine) { Get-ServiceLog | Where-Object { $_ -match 'input desktop Default' -and (& $stamp $_) -gt (& $stamp $sasLine) } | Select-Object -First 1 }
    $backAfter = if ($sasLine -and $backLine) { ((& $stamp $backLine) - (& $stamp $sasLine)).TotalSeconds } else { -1 }
    "helper injected: $injected; the own desktop was back $([math]::Round($backAfter, 2)) s after Ctrl+Alt+Del"
    Add-Check 'helper: injected Escape (down and up)' ($injected -ge 2)
    Add-Check 'Escape closed the screen (back in 1.5..4 s)' (($backAfter -ge 1.5) -and ($backAfter -lt 4))
    # The PC must not be left on the Ctrl+Alt+Del screen.
    $lastDesktop = @(Get-ServiceLog | Where-Object { $_ -match 'input desktop (\S+)' } | ForEach-Object { [regex]::Match($_, 'input desktop (\S+)').Groups[1].Value }) | Select-Object -Last 1
    "last input desktop the helper saw: $lastDesktop"
    if ($lastDesktop -ne 'Default') { $failed += 'left on another desktop'; 'WARNING: the PC may still show the Ctrl+Alt+Del screen' }

    '=== 4. Restart Queqiao on that PC: the service starts the agent again'
    $agentArgs2 = "--server $listen --pin $pin --user alice --password $password --sessions 2 --seconds 150 --report `"$Out\agent_restart.txt`""
    # Installing again starts the service's folder, and so its log, anew: count from here.
    $logSince = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
    $startsBefore = 0
    & { $ErrorActionPreference = 'Continue'; & $serviceExe install --agent-arguments $agentArgs2 --no-restart 2>&1 | ForEach-Object { "$_" } }
    if (-not (Wait-ServiceLog 'started the desktop helper' ($startsBefore + 1) 30)) { $failed += 'service start 2' }
    Start-Sleep -Seconds 3
    Start-SessionTask 'Queqiao-M5-Restart' $viewer "--server $listen --pin $pin --user alice --password $password --device $device --seconds 8 --command restart-queqiao --width 960 --mute --report `"$Out\viewer_restart.txt`""
    [void](Wait-SessionTask 'Queqiao-M5-Restart' 120)
    $restarted = Wait-ServiceLog 'started the desktop helper' ($startsBefore + 2) 30
    $asked = (Get-ServiceLog | Where-Object { $_ -match 'the agent asked to restart Queqiao' }).Count -gt 0
    $viewerRestart = @(if (Test-Path -LiteralPath "$Out\viewer_restart.txt") { @(Get-Content -LiteralPath "$Out\viewer_restart.txt" -Encoding UTF8) } else { @('no viewer report') })
    $viewerRestart | Where-Object { $_ -match 'restart|Queqiao|ended|RESULT' }
    Add-Check 'service: the agent asked to restart' $asked
    Add-Check 'service: helper and agent started again' $restarted
    $completed = $true
} finally {
    '=== service log (this run)'
    Get-ServiceLog
    if (-not $Keep) {
        & { $ErrorActionPreference = 'Continue'; & $serviceExe uninstall 2>&1 | ForEach-Object { "$_" } }
    }
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    if (-not $completed -and -not $failed.Count) { $failed += 'stopped early' }
    if ($failed.Count) { "M5_SERVICE_FAIL $($failed -join ', ')" } else { 'M5_SERVICE_PASS' }
}
