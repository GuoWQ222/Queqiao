# Runs a program in the interactive session of the user logged on at the console, and waits for it.
# Screen capture, windows and input injection only work there; a process started over SSH runs in session 0.
# Uses a one-off scheduled task with an interactive-only principal (no stored password), removed afterwards.
# Usage: scripts\run_in_session.ps1 -Exe <full path> [-Arguments '<arguments>'] [-TimeoutSec 60]
# Prints RUN_DONE exit <code> or RUN_FAIL <reason>.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Arguments = '',
    [int]$TimeoutSec = 60
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is logged on at the console'; return }
if (-not (Test-Path -LiteralPath $Exe)) { "RUN_FAIL not found: $Exe"; return }

$taskName = 'Queqiao-RunInSession'
$workDir = Split-Path -Parent $Exe
if ($Arguments) {
    $action = New-ScheduledTaskAction -Execute $Exe -Argument $Arguments -WorkingDirectory $workDir
} else {
    $action = New-ScheduledTaskAction -Execute $Exe -WorkingDirectory $workDir
}
$principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
$limit = New-TimeSpan -Seconds ($TimeoutSec + 30)
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit $limit
Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
try {
    Start-ScheduledTask -TaskName $taskName
    # LastTaskResult 267011 = has not run yet, 267009 = running; anything else is the program's exit code.
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        Start-Sleep -Milliseconds 300
        $result = (Get-ScheduledTaskInfo -TaskName $taskName).LastTaskResult
        $state = (Get-ScheduledTask -TaskName $taskName).State
        $busy = ($state -eq 'Running') -or ($result -eq 267009) -or ($result -eq 267011)
    } while ($busy -and (Get-Date) -lt $deadline)
    if ($busy) {
        Stop-ScheduledTask -TaskName $taskName
        "RUN_FAIL timeout after $TimeoutSec s (state $state, result $result)"
    } else {
        "RUN_DONE exit $result"
    }
} finally {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}
