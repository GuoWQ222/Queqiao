# M2 check on one PC: queqiao-viewer and queqiao-agent as two programs talking WebRTC over 127.0.0.1, both
# in the logged-on user's desktop session (one-off scheduled tasks, as run_in_session.ps1). The viewer sends
# the input test (five pointer moves, F24 pressed once); the agent checks it and puts the pointer back.
# Usage: scripts\run_m2_local.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 12] [-Force '']
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m2_local'),
    [int]$Seconds = 12,
    [string]$Force = '',
    [string]$Address = '127.0.0.1:47110'
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is logged on at the console'; return }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item -Path "$Out\*.txt" -ErrorAction SilentlyContinue

function Start-SessionTask([string]$Name, [string]$Exe, [string]$Arguments, [string]$Environment) {
    # QUEQIAO_FORCE goes through cmd /c set ... when given (a scheduled task has no environment of its own).
    if ($Environment) {
        $action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c set QUEQIAO_FORCE=$Environment&& `"$Exe`" $Arguments"
    } else {
        $action = New-ScheduledTaskAction -Execute $Exe -Argument $Arguments -WorkingDirectory (Split-Path -Parent $Exe)
    }
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds ($Seconds + 120)) -Priority 4
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

# --mute: viewer and agent run on this one PC. Played here, the sound the agent captures from this PC's speakers would
# be captured again and go round (a beep repeated for the whole session, heard on sunshine on 2026-11-22).
$viewerArgs = "--listen $Address --seconds $Seconds --report `"$Out\viewer.txt`" --input-test --width 960 --mute"
$agentArgs = "--connect $Address --seconds $Seconds --report `"$Out\agent.txt`" --verify-input"
Start-SessionTask 'Queqiao-M2-Viewer' (Join-Path $Bin 'queqiao-viewer.exe') $viewerArgs $Force
Start-Sleep -Milliseconds 800
Start-SessionTask 'Queqiao-M2-Agent' (Join-Path $Bin 'queqiao-agent.exe') $agentArgs $Force
$agentResult = Wait-SessionTask 'Queqiao-M2-Agent' ($Seconds + 90)
$viewerResult = Wait-SessionTask 'Queqiao-M2-Viewer' 30
"agent exit $agentResult, viewer exit $viewerResult"
foreach ($program in 'agent', 'viewer') {
    "=== $program"
    $report = Join-Path $Out "$program.txt"
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report -Encoding UTF8 } else { "no report at $report" }
}
