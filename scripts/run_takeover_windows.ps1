# One device controls a PC at a time (the user, 2026-11-26, as UU Remote), end to end on Windows (run as an
# administrator; the programs run in the signed-in person's session, as run_m7_agent_login.ps1 does): a queqiao-server
# of its own on 127.0.0.1, a throwaway agent device pcA, viewers of the same account (--test-device):
#   1. viewer A connects; the device list says pcA is controlled by viewerA; another device's main window is pictured
#      (main_devices.bmp: the being-controlled banner and the list's chip);
#   2. viewer B without taking over: told viewerA controls it;
#   3. viewer B taking over: A is told viewerB took over; the list says viewerB;
#   4. a third device ends B's session (the banner's Disconnect): B is told.
# No input test: the pointer is not moved. Prints the steps and TAKEOVER_WIN_PASS or TAKEOVER_WIN_FAIL. ASCII only.
param([string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'), [int]$Port = 47140)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\probe\takeover'
if (Test-Path -LiteralPath $out) { Remove-Item -Recurse -Force -LiteralPath $out }
New-Item -ItemType Directory -Force $out | Out-Null
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'TAKEOVER_WIN_FAIL nobody is signed in at the console'; return }
$failed = @()
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-62} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}
function Test-Report([string]$File, [string]$Text) {
    (Test-Path -LiteralPath $File) -and (Select-String -LiteralPath $File -SimpleMatch -Pattern $Text -Quiet)
}
# The session ended with the agent's notice naming $Device (in any language).
function Test-Ended([string]$File, [string]$Device) {
    (Test-Path -LiteralPath $File) -and (Select-String -LiteralPath $File -Encoding UTF8 -Pattern "^ended: the agent ended the session: .*\b$Device\b" -Quiet)
}

$server = Join-Path $Bin 'queqiao-server.exe'
$viewer = Join-Path $Bin 'queqiao-viewer.exe'
$data = Join-Path $out 'server-data'
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "TAKEOVER_WIN_FAIL no fingerprint from: $init"; return }
$password = [guid]::NewGuid().ToString('N')
$password | & $server add-user tester --data $data | Out-Null
$address = "127.0.0.1:$Port"
$account = "--server $address --pin $pin --user tester --password $password"
$serverProcess = Start-Process -FilePath $server -ArgumentList @('serve', '--listen', $address, '--data', "`"$data`"", '--public-ip', '127.0.0.1', '--turn-port', '0') -PassThru -WindowStyle Hidden -RedirectStandardOutput "$out\server.txt" -RedirectStandardError "$out\server_err.txt"
Start-Sleep -Seconds 2

# A program in the signed-in person's session (a scheduled task of its own).
$tasks = @()
function Start-InSession([string]$Name, [string]$Exe, [string]$Arguments) {
    $action = New-ScheduledTaskAction -Execute $Exe -Argument $Arguments
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 180)
    Register-ScheduledTask -TaskName "Queqiao-takeover-$Name" -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName "Queqiao-takeover-$Name"
    $script:tasks += "Queqiao-takeover-$Name"
}
function Wait-Report([string]$File, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline -and -not (Test-Path -LiteralPath $File)) { Start-Sleep -Milliseconds 500 }
}
function Get-Listed([string]$Name) {
    $report = Join-Path $out "list_$Name.txt"
    & $viewer --test-device lister --list --report $report $account.Split(' ') | Out-Null
    $line = if (Test-Path -LiteralPath $report) { Select-String -LiteralPath $report -Pattern 'device pcA ' | Select-Object -First 1 } else { $null }
    "list: $line"
    $report
}

try {
    Start-InSession 'agent' (Join-Path $Bin 'queqiao-agent.exe') "--test-device pcA $account --sessions 3 --seconds 150 --report `"$out\agent.txt`""
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline -and -not (Select-String -Path "$out\server*.txt" -Pattern 'pcA' -Quiet)) { Start-Sleep -Milliseconds 300 }

    # 1. Viewer A controls pcA.
    Start-InSession 'viewerA' $viewer "--test-device viewerA $account --device pcA --seconds 70 --width 640 --report `"$out\viewer_a.txt`""
    Start-Sleep -Seconds 10
    $listed = @(Get-Listed '1')
    $listed[0]
    Add-Check 'the list says viewerA controls pcA' (Test-Report $listed[-1] 'controlled by viewerA')
    Start-InSession 'panel' (Join-Path $Bin 'queqiao-agent.exe') "--test-device panelpc $account --test-main-window --ui-pictures `"$out`" --seconds 9 --report `"$out\agent_panel.txt`""
    Wait-Report "$out\agent_panel.txt" 30
    Add-Check "another device's main window pictured" (Test-Report "$out\agent_panel.txt" 'main window: pictures of its pages saved')

    # 2. Viewer B without taking over.
    Start-InSession 'viewerB1' $viewer "--test-device viewerB $account --device pcA --seconds 5 --width 640 --report `"$out\viewer_b1.txt`""
    Wait-Report "$out\viewer_b1.txt" 40
    Add-Check 'B without taking over is told viewerA controls it' (Test-Report "$out\viewer_b1.txt" 'is busy: viewerA controls it')

    # 3. Viewer B takes control.
    Start-InSession 'viewerB2' $viewer "--test-device viewerB $account --device pcA --take-over --seconds 60 --width 640 --report `"$out\viewer_b2.txt`""
    Wait-Report "$out\viewer_a.txt" 40
    # The agent's words are in its PC's language (on a Chinese Windows the sentence is Chinese, with "viewerB" in it): the end and the name.
    Add-Check 'A is told viewerB took over' (Test-Ended "$out\viewer_a.txt" 'viewerB')
    Start-Sleep -Seconds 8
    $listed = @(Get-Listed '2')
    $listed[0]
    Add-Check 'the list says viewerB controls pcA' (Test-Report $listed[-1] 'controlled by viewerB')

    # 4. Another device ends B's session.
    & $viewer --test-device ender --end-control pcA --report "$out\end.txt" $account.Split(' ') | Out-Null
    Wait-Report "$out\viewer_b2.txt" 40
    Add-Check 'B is told ender ended it' (Test-Ended "$out\viewer_b2.txt" 'ender')
    Start-Sleep -Seconds 3
    $listed = @(Get-Listed '3')
    $listed[0]
    Add-Check 'the list names nobody once it ended' (-not (Test-Report $listed[-1] 'controlled by'))
} finally {
    Start-Sleep -Milliseconds 500
    foreach ($task in $tasks) {
        Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    }
    Get-Process queqiao-agent, queqiao-viewer -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Bin\*" } | Stop-Process -Force
    if ($serverProcess) { Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue }
}
if ($failed.Count -eq 0) { 'TAKEOVER_WIN_PASS' } else { 'TAKEOVER_WIN_FAIL ' + ($failed -join '; ') }
