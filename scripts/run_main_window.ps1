# The main window, as the person at this PC sees it: queqiao-server on this PC (fresh data folder, one user), a
# second device of the account (a viewer logged in as "Office PC", which then leaves: shown offline), a few files in
# the list of transfers, and the agent logged in with --test-main-window, which saves a picture of each page
# (main_devices.bmp, main_help.bmp, main_transfers.bmp, main_settings.bmp) into -Out. The list of transfers is the
# test build's own (transfers.test.log), put back as it was afterwards. Runs in the console session (one-off scheduled tasks); the window shows there for
# a few seconds. Prints MAIN_WINDOW_PASS or MAIN_WINDOW_FAIL. ASCII only.
# Usage: scripts\run_main_window.ps1 [-Bin target\release] [-Out <folder>]
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\main_window'),
    [string]$Listen = '127.0.0.1:47100',
    # The windows' language for this run (en or zh: QUEQIAO_LANG for the agent); empty: as the PC has it.
    [string]$Lang = ''
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
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 240) -Priority 4
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
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "RUN_FAIL no fingerprint from: $init"; return }
$password = [guid]::NewGuid().ToString('N')
$password | & $server add-user alice --data $data | Out-Null
$serveArgs = @('serve', '--listen', $Listen, '--data', "`"$data`"", '--public-ip', $Listen.Split(':')[0], '--relay-private')
$serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
$hostName, $port = $Listen.Split(':')
$deadline = (Get-Date).AddSeconds(10)
do {
    Start-Sleep -Milliseconds 200
    $client = New-Object Net.Sockets.TcpClient
    try { $client.Connect($hostName, [int]$port); $up = $true } catch { $up = $false } finally { $client.Dispose() }
} while (-not $up -and (Get-Date) -lt $deadline)
if (-not $up) { 'RUN_FAIL the server did not start'; return }

# The agent keeps its files in a folder of the test's own (APPDATA): its settings test turns a setting over and
# changes the display language, which must never reach the person's own Queqiao on this PC (2026-11-26).
$appData = Join-Path $Out 'appdata'
if (Test-Path -LiteralPath $appData) { Remove-Item -Recurse -Force -LiteralPath $appData }
New-Item -ItemType Directory -Force $appData | Out-Null
# A test build reads transfers.test.log there: a sample.
$history = Join-Path $appData 'Queqiao\transfers.test.log'
$kept = if (Test-Path -LiteralPath $history) { [IO.File]::ReadAllBytes($history) } else { $null }
$sample = @(
    "2026-11-22 09:41`tS`t2483112`tOffice PC`tquarterly-report.xlsx",
    "2026-11-22 10:05`tR`t15728640`tOffice PC`tdesign-review.pdf",
    "2026-11-22 11:30`tR`t48213`tOffice PC`tnotes.txt"
)
New-Item -ItemType Directory -Force (Split-Path $history) | Out-Null
[IO.File]::WriteAllText($history, ($sample -join "`n") + "`n")
$failed = @()
try {
    # A second device of the account: logged in once, then gone (offline in the list).
    & $viewer --server $Listen --pin $pin --user alice --password $password --test-device 'Office-PC' --list --report "$Out\viewer_list.txt" | Out-Null
    $pictures = $Out
    $agentArgs = "--server $Listen --pin $pin --user alice --password $password --seconds 25 --help-code K7Q2M9XD --test-main-window --ui-pictures `"$pictures`" --report `"$Out\agent.txt`""
    if ($Lang -and $Lang -notmatch '^(en|zh)$') { throw '-Lang: en or zh' }
    $language = if ($Lang) { "set QUEQIAO_LANG=$Lang&& " } else { '' }
    Start-SessionTask 'Queqiao-MainWindow' "$env:SystemRoot\System32\cmd.exe" "/c set `"APPDATA=$appData`"&& $language`"$agent`" $agentArgs"
    $result = Wait-SessionTask 'Queqiao-MainWindow' 90
    "agent exit $result"
    $report = Join-Path $Out 'agent.txt'
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report -Encoding UTF8 | Where-Object { $_ -match 'main window|logged in|ERROR|RESULT' } } else { 'no agent report' }
    foreach ($page in 'devices', 'help', 'transfers', 'settings', 'settings_security', 'settings_sessions', 'settings_keyboard') {
        $picture = Join-Path $Out "main_$page.bmp"
        $ok = (Test-Path -LiteralPath $picture) -and ((Get-Item -LiteralPath $picture).Length -gt 100000)
        '{0,-40} {1}' -f "picture of the $page page", $(if ($ok) { 'ok' } else { 'FAIL' })
        if (-not $ok) { $failed += $page }
    }
    # The Settings page's test: its controls answer clicks where they are drawn; a click on a switch reached the
    # agent; choosing English drew the windows in it (unless QUEQIAO_LANG holds Chinese, which wins).
    $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @() }
    $text | Where-Object { $_ -match '^settings test|set: ask before' }
    $wanted = @('^settings test: \d+ controls answer clicks where they are drawn: yes$', '^settings test: clicked the switch of asking before each session: true$', '^the person at this PC set: ask before each session: ')
    if ($Lang -ne 'zh') { $wanted += "^settings test: display language \w+ -> English after choosing English, the main window's title: Queqiao$" }
    foreach ($pattern in $wanted) {
        if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "settings ($pattern)" }
    }
} finally {
    if ($null -eq $kept) { Remove-Item -LiteralPath $history -ErrorAction SilentlyContinue } else { [IO.File]::WriteAllBytes($history, $kept) }
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
}
if ($failed.Count) { "MAIN_WINDOW_FAIL $($failed -join ', ')" } else { 'MAIN_WINDOW_PASS' }
