# M3 check on one PC, through queqiao-server on this PC with a fresh data folder and two new users:
# 1. the account probe (certificate pin, logins, tokens, presence, the same-account rule), as a console
#    program;
# 2. the agent logged in to the account and waiting, and the viewer going through the login form and the
#    device list as a person would (filled and pressed by the program), controlling this PC through the
#    server: video, and the input test checked by the agent; pictures of both forms are saved;
# 3. the saved login: the viewer lists the devices with no password, logs out, and is refused afterwards.
# The agent and viewer run in the logged-on user's desktop session (one-off scheduled tasks).
# Usage: scripts\run_m3_local.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 12]
# Prints M3_LOCAL_PASS or M3_LOCAL_FAIL: <what failed> last (RUN_FAIL before it when it could not start).
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m3_local'),
    [int]$Seconds = 12,
    [string]$Listen = '127.0.0.1:47100'
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is logged on at the console'; return }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item -Path "$Out\*.txt", "$Out\*.bmp" -ErrorAction SilentlyContinue
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

function Show-Report([string]$Program) {
    "=== $Program"
    $report = Join-Path $Out "$Program.txt"
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report -Encoding UTF8 } else { "no report at $report" }
}

$server = Join-Path $Bin 'queqiao-server.exe'
$viewer = Join-Path $Bin 'queqiao-viewer.exe'
$agent = Join-Path $Bin 'queqiao-agent.exe'
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "RUN_FAIL no fingerprint from: $init"; return }
# Throw-away users with random passwords (the data folder is new each run).
$password1 = [guid]::NewGuid().ToString('N')
$password2 = [guid]::NewGuid().ToString('N')
$password1 | & $server add-user alice --data $data
$password2 | & $server add-user bob --data $data
$serveArgs = @('serve', '--listen', $Listen, '--data', "`"$data`"", '--public-ip', $Listen.Split(':')[0], '--relay-private')
$hostName, $port = $Listen.Split(':')
function Start-TestServer([string]$Log) {
    $process = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\$Log" -RedirectStandardError "$Out\server_err.txt"
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect($hostName, [int]$port); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if ($up) { $process } else { $null }
}
# What failed, for the verdict at the end; $reached: the checks ran to the end (else RUN_FAIL said why).
$failures = New-Object System.Collections.Generic.List[string]
$reached = $false
function Test-Report([string]$Program, [string]$Expected) {
    $report = Join-Path $Out "$Program.txt"
    $lines = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @() }
    if (-not ($lines -match $Expected)) { $failures.Add("$Program (expected '$Expected')") }
}
$serverProcess = Start-TestServer 'server_out.txt'
try {
    if (-not $serverProcess) { 'RUN_FAIL the server did not start'; return }
    "server up, certificate $pin"

    '=== account probe'
    $probe = @(& (Join-Path $Bin 'examples\account_probe.exe') $Listen $pin alice $password1 bob $password2)
    $probe
    if ($probe -notcontains 'PROBE_PASS') { $failures.Add('account probe') }
    # The probe fails logins on purpose (lockout checks, counted for 127.0.0.1 too): the server starts again so its
    # in-memory failure counts do not lock the sessions out (as in run_m4_local.ps1). Accounts stay in its database.
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    $serverProcess = Start-TestServer 'server_out2.txt'
    if (-not $serverProcess) { 'RUN_FAIL the server did not start again'; return }
    # Sign-up and the account hardening apart, against the server just started (the checks above used up this
    # address's tries); then once more anew for the sessions.
    $probe = @(& (Join-Path $Bin 'examples\account_probe.exe') $Listen $pin alice $password1 bob $password2 --accounts)
    $probe
    if ($probe -notcontains 'PROBE_PASS') { $failures.Add('account probe --accounts') }
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    $serverProcess = Start-TestServer 'server_out3.txt'
    if (-not $serverProcess) { 'RUN_FAIL the server did not start a third time'; return }

    $device = $env:COMPUTERNAME
    $agentArgs = "--server $Listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --verify-input --report `"$Out\agent.txt`""
    # --mute: viewer and agent run on this one PC. Played here, the sound the agent captures from this PC's speakers
    # would be captured again and go round (a beep repeated for the whole session, heard on sunshine on 2026-11-22).
    $viewerArgs = "--server $Listen --pin $pin --user alice --password $password1 --ui-test $device --seconds $Seconds --input-test --width 960 --mute --report `"$Out\viewer.txt`""
    Start-SessionTask 'Queqiao-M3-Agent' $agent $agentArgs
    Start-Sleep -Seconds 2
    Start-SessionTask 'Queqiao-M3-Viewer' $viewer $viewerArgs
    $viewerResult = Wait-SessionTask 'Queqiao-M3-Viewer' ($Seconds + 120)
    $agentResult = Wait-SessionTask 'Queqiao-M3-Agent' 30
    "agent exit $agentResult, viewer exit $viewerResult"
    Show-Report 'agent'
    Show-Report 'viewer'

    # The saved login: no password needed; after logging out it is refused.
    foreach ($step in @(@('list_saved', '--list'), @('logout', '--logout'), @('list_after_logout', '--list'))) {
        Start-SessionTask "Queqiao-M3-$($step[0])" $viewer "$($step[1]) --report `"$Out\$($step[0]).txt`""
        $result = Wait-SessionTask "Queqiao-M3-$($step[0])" 60
        "$($step[0]) exit $result"
        Show-Report $step[0]
    }
    Test-Report 'agent' '^RESULT PASS'
    Test-Report 'viewer' '^RESULT PASS'
    Test-Report 'list_saved' '^RESULT PASS'
    Test-Report 'logout' '^RESULT PASS'
    # After logging out the saved login is gone: the list must be refused.
    Test-Report 'list_after_logout' '^ERROR not logged in'
    $reached = $true
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_out.txt", "$Out\server_out2.txt", "$Out\server_err.txt" -ErrorAction SilentlyContinue
    if (-not $reached) { 'M3_LOCAL_FAIL: the checks did not run to the end' }
    elseif ($failures.Count) { 'M3_LOCAL_FAIL: ' + ($failures -join '; ') }
    else { 'M3_LOCAL_PASS' }
}
