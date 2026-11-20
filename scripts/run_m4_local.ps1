# M4 check on one PC: queqiao-server with STUN/TURN (fresh data folder, two new users), then
# 1. the account probe, including the relay checks (credentials, where it relays, quota, relaying);
# 2. a session as usual (expected: direct);
# 3. a session with QUEQIAO_FORCE=network:relay on both sides (expected: relayed through the server).
# The agent and viewer run in the logged-on user's desktop session (one-off scheduled tasks). Every session
# checks video and the input test. Prints the reports and M4_LOCAL_PASS or M4_LOCAL_FAIL.
# Usage: scripts\run_m4_local.ps1 [-Bin target\release] [-Out <report folder>] [-Seconds 10] [-Address 127.0.0.1]
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m4_local'),
    [int]$Seconds = 10,
    [string]$Address = '127.0.0.1',
    # Which sessions to run: direct, relay.
    [string[]]$Sessions = @('direct', 'relay'),
    [switch]$SkipProbe
)
$ErrorActionPreference = 'Stop'
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is logged on at the console'; return }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item -Path "$Out\*.txt" -ErrorAction SilentlyContinue
$data = Join-Path $Out 'server-data'
if (Test-Path -LiteralPath $data) { Remove-Item -Recurse -Force -LiteralPath $data }

function Start-SessionTask([string]$Name, [string]$Exe, [string]$Arguments, [string]$Force) {
    # QUEQIAO_FORCE goes through cmd /c set ... when given (a scheduled task has no environment of its own).
    if ($Force) {
        $action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c set QUEQIAO_FORCE=$Force&& `"$Exe`" $Arguments"
    } else {
        $action = New-ScheduledTaskAction -Execute $Exe -Argument $Arguments -WorkingDirectory (Split-Path -Parent $Exe)
    }
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

$server = Join-Path $Bin 'queqiao-server.exe'
$viewer = Join-Path $Bin 'queqiao-viewer.exe'
$agent = Join-Path $Bin 'queqiao-agent.exe'
$listen = "${Address}:47100"
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "RUN_FAIL no fingerprint from: $init"; return }
$password1 = [guid]::NewGuid().ToString('N')
$password2 = [guid]::NewGuid().ToString('N')
& $server add-user alice $password1 --data $data | Out-Null
& $server add-user bob $password2 --data $data | Out-Null
$serveArgs = @('serve', '--listen', $listen, '--data', "`"$data`"", '--public-ip', $Address, '--relay-private')
$serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
$failed = @()
try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect($Address, 47100); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if (-not $up) { 'RUN_FAIL the server did not start'; return }
    Get-Content -LiteralPath "$Out\server_out.txt"

    if (-not $SkipProbe) {
        '=== account and relay probe'
        $probe = & (Join-Path $Bin 'examples\account_probe.exe') $listen $pin alice $password1 bob $password2
        $probe
        if ($probe[-1] -ne 'PROBE_PASS') { $failed += 'probe' }
    }

    $device = $env:COMPUTERNAME
    foreach ($run in @(@('direct', '', 'direct to'), @('relay', 'network:relay', 'relayed through'))) {
        $label, $mode, $expected = $run
        if ($Sessions -notcontains $label) { continue }
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --verify-input --report `"$Out\agent_$label.txt`""
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --device $device --seconds $Seconds --input-test --width 960 --report `"$Out\viewer_$label.txt`""
        Start-SessionTask "Queqiao-M4-Agent-$label" $agent $agentArgs $mode
        Start-Sleep -Seconds 2
        Start-SessionTask "Queqiao-M4-Viewer-$label" $viewer $viewerArgs $mode
        $viewerResult = Wait-SessionTask "Queqiao-M4-Viewer-$label" ($Seconds + 120)
        $agentResult = Wait-SessionTask "Queqiao-M4-Agent-$label" 30
        "=== session '$label' (QUEQIAO_FORCE=$mode): agent exit $agentResult, viewer exit $viewerResult"
        foreach ($program in 'agent', 'viewer') {
            $report = Join-Path $Out "${program}_$label.txt"
            $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
            "--- $program"
            $text
            $pathLine = $text | Where-Object { $_ -like 'path after*' } | Select-Object -Last 1
            if ($text[-1] -ne 'RESULT PASS') { $failed += "$label $program" }
            if (-not $pathLine -or $pathLine -notlike "*: $expected *") { $failed += "$label $program path (expected '$expected')" }
        }
    }
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_err.txt" -ErrorAction SilentlyContinue | Select-Object -Last 20
}
if ($failed) { "M4_LOCAL_FAIL: $($failed -join '; ')" } else { 'M4_LOCAL_PASS' }
