# M3 check on one PC, through queqiao-server on this PC with a fresh data folder and two new users:
# 1. the account probe (certificate pin, logins, tokens, presence, the same-account rule), as a console
#    program;
# 2. the agent logged in to the account and waiting, and the viewer going through the login form and the
#    device list as a person would (filled and pressed by the program), controlling this PC through the
#    server: video, and the input test checked by the agent; pictures of both forms are saved;
# 3. the saved login: the viewer lists the devices with no password, logs out, and is refused afterwards.
# The agent and viewer run in the logged-on user's desktop session (one-off scheduled tasks).
# Usage: scripts\run_m3_local.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 12]
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
& $server add-user alice $password1 --data $data
& $server add-user bob $password2 --data $data
$serverProcess = Start-Process -FilePath $server -ArgumentList @('serve', '--listen', $Listen, '--data', "`"$data`"") -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
try {
    $hostName, $port = $Listen.Split(':')
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect($hostName, [int]$port); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if (-not $up) { 'RUN_FAIL the server did not start'; return }
    "server up, certificate $pin"

    '=== account probe'
    & (Join-Path $Bin 'examples\account_probe.exe') $Listen $pin alice $password1 bob $password2

    $device = $env:COMPUTERNAME
    $agentArgs = "--server $Listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --verify-input --report `"$Out\agent.txt`""
    $viewerArgs = "--server $Listen --pin $pin --user alice --password $password1 --ui-test $device --seconds $Seconds --input-test --width 960 --report `"$Out\viewer.txt`""
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
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_out.txt", "$Out\server_err.txt" -ErrorAction SilentlyContinue
}
