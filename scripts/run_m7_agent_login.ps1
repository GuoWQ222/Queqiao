# M7 check of the agent when it is not online (run as an administrator; the agent runs in the signed-in person's
# session): a local queqiao-server with a throwaway user; an agent that starts not logged in, logs in through the
# tray's "Log in..." and its form (a throwaway test device, so this PC's own login stays as it is); then the server
# is stopped and started again: the agent says it lost the connection and is back online by itself.
# Prints M7_LOGIN_PASS or M7_LOGIN_FAIL. ASCII only.
param([string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'), [int]$Port = 47130)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\probe\m7_login'
if (Test-Path -LiteralPath $out) { Remove-Item -Recurse -Force -LiteralPath $out }
New-Item -ItemType Directory -Force $out | Out-Null
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
$failed = @()
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-58} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}

# The PC's own saved agent login (it must not change).
$sid = (New-Object Security.Principal.NTAccount $consoleUser).Translate([Security.Principal.SecurityIdentifier]).Value
$userHome = (Get-CimInstance Win32_UserProfile | Where-Object { $_.SID -eq $sid }).LocalPath
$ownLogin = Join-Path $userHome 'AppData\Roaming\Queqiao\agent.json'
function Get-OwnLogin { if (Test-Path -LiteralPath $ownLogin) { (Get-FileHash -LiteralPath $ownLogin).Hash } else { 'none' } }
$ownBefore = Get-OwnLogin

$server = Join-Path $Bin 'queqiao-server.exe'
$data = Join-Path $out 'server-data'
$init = & $server init --data $data
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "M7_LOGIN_FAIL no fingerprint from: $init"; return }
$password = [guid]::NewGuid().ToString('N')
$password | & $server add-user tester --data $data | Out-Null
$address = "127.0.0.1:$Port"
function Start-Server([string]$Name) {
    $process = Start-Process -FilePath $server -ArgumentList @('serve', '--listen', $address, '--data', "`"$data`"", '--turn-port', '0') -PassThru -WindowStyle Hidden -RedirectStandardOutput "$out\server_$Name.txt" -RedirectStandardError "$out\server_${Name}_err.txt"
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect('127.0.0.1', $Port); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if ($up) { $process } else { $null }
}

$first = Start-Server 'first'
if (-not $first) { 'M7_LOGIN_FAIL the server did not start'; return }
$report = Join-Path $out 'agent.txt'
$arguments = "--test-device m7-login --test-tray-login --server $address --pin $pin --user tester --password $password --seconds 50 --ui-pictures `"$out`" --report `"$report`""
$action = New-ScheduledTaskAction -Execute (Join-Path $Bin 'queqiao-agent.exe') -Argument $arguments
$principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 120)
Register-ScheduledTask -TaskName 'Queqiao-M7-login' -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName 'Queqiao-M7-login'
try {
    # Logged in through the form: the server log shows the device.
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline -and -not (Select-String -Path "$out\server_first*.txt" -Pattern 'm7-login' -Quiet)) { Start-Sleep -Milliseconds 300 }
    Start-Sleep -Seconds 3
    '--- the server stops'
    Stop-Process -Id $first.Id -Force
    Start-Sleep -Seconds 4
    '--- the server starts again'
    $second = Start-Server 'second'
    Add-Check 'the server started again' ($null -ne $second)
    $deadline = (Get-Date).AddSeconds(90)
    while ((Get-Date) -lt $deadline -and -not (Test-Path -LiteralPath $report)) { Start-Sleep -Seconds 1 }
} finally {
    Start-Sleep -Milliseconds 500
    Unregister-ScheduledTask -TaskName 'Queqiao-M7-login' -Confirm:$false
    Get-Process queqiao-server -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $server } | Stop-Process -Force
}
$text = @(if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report) } else { @('no report') })
'--- agent'
$text | Where-Object { $_ -notmatch 'password' }
Add-Check 'started not logged in' (@($text | Where-Object { $_ -like 'not logged in: Log in...*' }).Count -eq 1)
Add-Check 'logged in through the tray and its form' (@($text | Where-Object { $_ -eq 'logged in from the tray' }).Count -eq 1)
Add-Check 'the login form was drawn' (Test-Path -LiteralPath (Join-Path $out 'agent_login.bmp'))
Add-Check 'it noticed the server going away' (@($text | Where-Object { $_ -like 'server connection lost*' }).Count -ge 1)
Add-Check 'back online by itself' (@($text | Where-Object { $_ -eq 'reconnected' }).Count -ge 1)
Add-Check 'online twice (the form, then the reconnect)' (@($text | Where-Object { $_ -like 'logged in as tester*' }).Count -eq 2)
Add-Check "this PC's own login unchanged" ((Get-OwnLogin) -eq $ownBefore)
if ($failed.Count) { "M7_LOGIN_FAIL $($failed -join ', ')" } else { 'M7_LOGIN_PASS' }
