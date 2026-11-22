# M7 check of updates through the Queqiao service on one PC (run as an administrator; uses run_m7_update.ps1's
# builds: target\release = 0.1.0, target\v011\release = 0.1.1, made there). Installs 0.1.0 for the signed-in person
# WITH the service (its agent runs from %ProgramFiles%\Queqiao), serves an update feed on 127.0.0.1, then
# 1. the service's agent finds 0.1.1, updates the person's installation, and hands the update to the service, which
#    checks it itself and runs its installer as SYSTEM: the service stopped, its programs replaced, started again,
#    its 0.1.1 agent running;
# 2. 0.1.2 with an agent that cannot start: the service's programs are put back (0.1.1) and it runs again; not retried;
# 3. uninstalled (the service too), the test registry values removed.
# Prints M7_SERVICE_UPDATE_PASS or M7_SERVICE_UPDATE_FAIL. ASCII only.
param(
    [string]$Key = (Join-Path $env:USERPROFILE '.queqiao-release\release_key.bin'),
    [int]$Port = 47141
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\probe\m7_service_update'
if (Test-Path -LiteralPath $out) { Remove-Item -Recurse -Force -LiteralPath $out }
$feed = Join-Path $out 'feed'
New-Item -ItemType Directory -Force $feed | Out-Null
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
if (Get-Service Queqiao -ErrorAction SilentlyContinue) { 'RUN_FAIL the Queqiao service is installed already: remove it first'; return }
$failed = @()
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-58} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}

$sid = (New-Object Security.Principal.NTAccount $consoleUser).Translate([Security.Principal.SecurityIdentifier]).Value
$userHome = (Get-CimInstance Win32_UserProfile | Where-Object { $_.SID -eq $sid }).LocalPath
$target = Join-Path $userHome 'AppData\Local\Programs\Queqiao'
$userKey = "Registry::HKEY_USERS\$sid\Software\Queqiao"
$uninstallKey = "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Uninstall\Queqiao"
$programFiles = Join-Path $env:ProgramFiles 'Queqiao'
$serviceUpdates = Join-Path $env:ProgramData 'Queqiao\updates'
$serviceLog = Join-Path $env:ProgramData 'Queqiao\service.log'
# This run's lines, by their time stamps (installing the service starts its folder, and so the log, anew).
$logSince = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')

$v1 = Join-Path $root 'target\release'
$v2 = Join-Path $root 'target\v011\release'
if (-not (Test-Path -LiteralPath (Join-Path $v2 'queqiao-setup.exe'))) { 'RUN_FAIL no 0.1.1 build: run scripts\run_m7_update.ps1 first'; return }
$installers = @{}
foreach ($bin in $v1, $v2) {
    $package = @(& (Join-Path $PSScriptRoot 'package.ps1') -Bin $bin)
    if ($package[-1] -notlike 'PACKAGE_PASS*') { $package; 'M7_SERVICE_UPDATE_FAIL packaging'; return }
    $file = ($package[-1] -split ' ')[1]
    $installers[[IO.Path]::GetFileNameWithoutExtension($file).Replace('queqiao-setup-', '')] = $file
}
function Get-Version([string]$Program) {
    if (-not (Test-Path -LiteralPath $Program)) { return 'none' }
    ((& $Program --version 2>$null | Select-Object -First 1) -split ' ')[1]
}
function Publish([string]$Installer, [string]$Version, [string]$Notes) {
    @(& (Join-Path $v1 'queqiao-setup.exe') release --key $Key --installer $Installer --version $Version --output $feed --notes $Notes 2>&1 | ForEach-Object { "$_" })
}
Publish $installers['0.1.1'] '0.1.1' 'Test update' | Out-Null

$requests = Join-Path $out 'requests.txt'
$server = Start-Job -ArgumentList $feed, $Port, $requests -ScriptBlock {
    param($FeedRoot, $Port, $Log)
    $listener = New-Object Net.HttpListener
    $listener.Prefixes.Add("http://127.0.0.1:$Port/")
    $listener.Start()
    while ($true) {
        $context = $listener.GetContext()
        $fileName = [IO.Path]::GetFileName($context.Request.Url.AbsolutePath)
        $path = Join-Path $FeedRoot $fileName
        Add-Content -LiteralPath $Log -Value $fileName
        if ($fileName -and (Test-Path -LiteralPath $path -PathType Leaf)) {
            $bytes = [IO.File]::ReadAllBytes($path)
            $context.Response.ContentLength64 = $bytes.Length
            $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
        } else {
            $context.Response.StatusCode = 404
        }
        $context.Response.Close()
    }
}
Start-Sleep -Seconds 2
New-Item -Path $userKey -Force | Out-Null
Set-ItemProperty -LiteralPath $userKey -Name UpdateFeed -Value "http://127.0.0.1:$Port/" -Type String
Set-ItemProperty -LiteralPath $userKey -Name UpdateFirstCheckSeconds -Value 20 -Type DWord
Remove-ItemProperty -LiteralPath $userKey -Name UpdateTried -ErrorAction SilentlyContinue

function Invoke-AsUser([string]$Name, [string]$Program, [string]$Arguments, [string]$Report, [int]$TimeoutSec = 180) {
    $action = New-ScheduledTaskAction -Execute $Program -Argument $Arguments
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSec)
    Register-ScheduledTask -TaskName "Queqiao-M7-$Name" -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName "Queqiao-M7-$Name"
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while (-not (Test-Path -LiteralPath $Report) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    Start-Sleep -Milliseconds 500
    Unregister-ScheduledTask -TaskName "Queqiao-M7-$Name" -Confirm:$false
}
function Wait-ServiceReport([datetime]$After, [int]$TimeoutSec) {
    $reportFile = Join-Path $serviceUpdates 'service_update.txt'
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path -LiteralPath $reportFile) -and (Get-Item -LiteralPath $reportFile).LastWriteTime -gt $After) {
            $text = @(Get-Content -LiteralPath $reportFile)
            if ($text -match '^RESULT ') { return $text }
        }
        Start-Sleep -Milliseconds 500
    }
    @('no service update report')
}
function Get-ServiceAgents { @(Get-Process queqiao-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$programFiles\*" }) }
function Get-ServiceLog { if (Test-Path -LiteralPath $serviceLog) { @(Get-Content -LiteralPath $serviceLog | Where-Object { $_.Length -ge 23 -and $_.Substring(0, 23) -ge $logSince }) } else { @() } }

try {
    '=== 1. 0.1.0 with the service, 0.1.1 on the feed'
    $installReport = Join-Path $out 'install.txt'
    $started = Get-Date
    Invoke-AsUser 'install' $installers['0.1.0'] "--quiet --service --report `"$installReport`"" $installReport
    Get-Content -LiteralPath $installReport -ErrorAction SilentlyContinue | Where-Object { $_ -match 'RESULT|ERROR|service|agent' }
    Start-Sleep -Seconds 3
    Add-Check 'the service runs 0.1.0 from Program Files' (((Get-Version (Join-Path $programFiles 'queqiao-service.exe')) -eq '0.1.0') -and (Get-ServiceAgents).Count -ge 1)
    $text = Wait-ServiceReport $started 150
    '--- service update report'; $text
    Start-Sleep -Seconds 5
    Add-Check 'service update: RESULT PASS' ($text -contains 'RESULT PASS')
    Add-Check 'service update: the service was stopped and started' (($text -contains 'the Queqiao service stopped') -and (@($text | Where-Object { $_ -like "the service's agent runs*" }).Count -eq 1))
    Add-Check 'service update: the earlier programs replaced' ($text -contains 'the earlier programs were replaced')
    Add-Check 'Program Files has 0.1.1 (service and agent)' (((Get-Version (Join-Path $programFiles 'queqiao-service.exe')) -eq '0.1.1') -and ((Get-Version (Join-Path $programFiles 'queqiao-agent.exe')) -eq '0.1.1'))
    Add-Check 'the service runs' ((Get-Service Queqiao -ErrorAction SilentlyContinue).Status -eq 'Running')
    Add-Check "the service's 0.1.1 agent runs" ((Get-ServiceAgents).Count -ge 1)
    Add-Check "the person's installation is 0.1.1 too" ((Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion -eq '0.1.1')
    Add-Check 'the service checked it itself (its log)' (@(Get-ServiceLog | Where-Object { $_ -like '*update to 0.1.1 checked (signed by the release key)*' }).Count -eq 1)
    Add-Check 'no copy of the earlier programs left' (-not (Test-Path -LiteralPath ($programFiles + '.old')))

    '=== 2. 0.1.2 whose agent cannot start'
    $broken = Join-Path $out 'broken'
    New-Item -ItemType Directory -Force $broken | Out-Null
    Set-Content -LiteralPath (Join-Path $broken 'queqiao-agent.exe') -Value 'not a program' -Encoding ascii
    $brokenInstaller = Join-Path $out 'broken-setup.exe'
    & (Join-Path $v2 'queqiao-setup.exe') pack --output $brokenInstaller (Join-Path $v2 'queqiao-viewer.exe') (Join-Path $broken 'queqiao-agent.exe') (Join-Path $v2 'queqiao-service.exe') | Out-Null
    Publish $brokenInstaller '0.1.2' 'Broken on purpose' | Out-Null
    # A fresh agent looks 20 s after it starts.
    $started = Get-Date
    Restart-Service Queqiao
    $text = Wait-ServiceReport $started 180
    '--- service update report'; $text
    Start-Sleep -Seconds 5
    Add-Check 'rollback: RESULT FAIL (the update did not hold)' ($text -contains 'RESULT FAIL')
    Add-Check 'rollback: no agent of the new version kept running' (@($text | Where-Object { $_ -like 'the new version did not keep running*' }).Count -eq 1)
    Add-Check 'rollback: the earlier version is back' ($text -contains 'the earlier version is back')
    Add-Check 'rollback: Program Files has 0.1.1 again' (((Get-Version (Join-Path $programFiles 'queqiao-service.exe')) -eq '0.1.1') -and ((Get-Version (Join-Path $programFiles 'queqiao-agent.exe')) -eq '0.1.1'))
    Add-Check 'rollback: the service runs' ((Get-Service Queqiao -ErrorAction SilentlyContinue).Status -eq 'Running')
    Add-Check "rollback: the service's 0.1.1 agent runs" ((Get-ServiceAgents).Count -ge 1)
    Add-Check 'rollback: no copy left over' (-not (Test-Path -LiteralPath ($programFiles + '.old')))
    $reportTime = (Get-Item -LiteralPath (Join-Path $serviceUpdates 'service_update.txt')).LastWriteTime
    Start-Sleep -Seconds 35
    $asked = @(Get-Content -LiteralPath $requests)
    Add-Check 'the agent that came back looked, did not take 0.1.2 again' ((@($asked | Where-Object { $_ -eq 'queqiao-setup-0.1.2.exe' }).Count -eq 1) -and ($asked[-1] -ne 'queqiao-setup-0.1.2.exe'))
    Add-Check 'nor did the service install it again' ((Get-Item -LiteralPath (Join-Path $serviceUpdates 'service_update.txt')).LastWriteTime -eq $reportTime)
} finally {
    '=== 3. uninstall'
    $uninstallReport = Join-Path $out 'uninstall.txt'
    if (Test-Path -LiteralPath (Join-Path $target 'queqiao-setup.exe')) {
        Invoke-AsUser 'uninstall' (Join-Path $target 'queqiao-setup.exe') "--uninstall --quiet --report `"$uninstallReport`"" $uninstallReport
    }
    Start-Sleep -Seconds 5
    Add-Check 'uninstall: RESULT PASS' (@(Get-Content -LiteralPath $uninstallReport -ErrorAction SilentlyContinue) -contains 'RESULT PASS')
    Add-Check 'uninstall: the service and its programs are gone' (-not (Get-Service Queqiao -ErrorAction SilentlyContinue) -and -not (Test-Path -LiteralPath $programFiles))
    foreach ($valueName in 'UpdateFeed', 'UpdateFirstCheckSeconds', 'UpdateTried') { Remove-ItemProperty -LiteralPath $userKey -Name $valueName -ErrorAction SilentlyContinue }
    Remove-Item -Recurse -Force -LiteralPath $serviceUpdates -ErrorAction SilentlyContinue
    Stop-Job $server -ErrorAction SilentlyContinue
    Remove-Job $server -Force -ErrorAction SilentlyContinue
    '--- service log'
    Get-ServiceLog | Select-Object -Last 30
}
if ($failed.Count) { "M7_SERVICE_UPDATE_FAIL $($failed -join ', ')" } else { 'M7_SERVICE_UPDATE_PASS' }
