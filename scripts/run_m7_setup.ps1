# M7 check of the installer on one PC (run as an administrator; the installer itself runs in the signed-in person's
# session, as they would run it): scripts\package.ps1 makes it, then
# 1. a quiet install with "start with Windows": the programs in %LOCALAPPDATA%\Programs\Queqiao, signed as the
#    package was, the Settings > Apps entry, the Start menu shortcut, the Run entry, the agent running from there;
# 2. the same again (an update): the earlier installation replaced, its running agent ended first;
# 3. a quiet uninstall: all of it gone;
# 4. an install with the service, then an uninstall: the service and %ProgramFiles%\Queqiao there and gone again.
# Prints M7_SETUP_PASS or M7_SETUP_FAIL. ASCII only.
param([string]$Installer = '', [int]$TimeoutSec = 120)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\probe\m7_setup'
New-Item -ItemType Directory -Force $out | Out-Null
Remove-Item -Path "$out\*.txt", "$out\*.bmp" -ErrorAction SilentlyContinue
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
$failed = @()
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-58} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}

if (-not $Installer) {
    $package = @(& (Join-Path $PSScriptRoot 'package.ps1'))
    $package | Where-Object { $_ -match 'PACKAGE_|signed by' }
    if ($package[-1] -notlike 'PACKAGE_PASS*') { 'M7_SETUP_FAIL packaging'; return }
    $Installer = ($package[-1] -split ' ')[1]
}
$certificate = (Get-AuthenticodeSignature -LiteralPath $Installer).SignerCertificate
"installer: $Installer, signed by $($certificate.Subject) ($($certificate.Thumbprint))"

# The person's folders, as the installer will see them in their session.
$userHome = (Get-CimInstance Win32_UserProfile | Where-Object { $_.SID -eq (New-Object Security.Principal.NTAccount $consoleUser).Translate([Security.Principal.SecurityIdentifier]).Value }).LocalPath
$target = Join-Path $userHome 'AppData\Local\Programs\Queqiao'
$shortcut = Join-Path $userHome 'AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Queqiao.lnk'
$sid = (New-Object Security.Principal.NTAccount $consoleUser).Translate([Security.Principal.SecurityIdentifier]).Value
$userKey = "Registry::HKEY_USERS\$sid"
"the person: $consoleUser, programs to $target"

function Invoke-Setup([string]$Name, [string]$Arguments) {
    $report = Join-Path $out "$Name.txt"
    $action = New-ScheduledTaskAction -Execute $Installer -Argument "$Arguments --report `"$report`""
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSec)
    Register-ScheduledTask -TaskName "Queqiao-M7-$Name" -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName "Queqiao-M7-$Name"
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while (-not (Test-Path -LiteralPath $report) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    Start-Sleep -Milliseconds 500
    Unregister-ScheduledTask -TaskName "Queqiao-M7-$Name" -Confirm:$false
    # Only the report comes back (the caller prints it).
    @(if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report) } else { @('no report') })
}
function Get-Installed {
    $entry = Get-ItemProperty -LiteralPath "$userKey\Software\Microsoft\Windows\CurrentVersion\Uninstall\Queqiao" -ErrorAction SilentlyContinue
    $run = (Get-ItemProperty -LiteralPath "$userKey\Software\Microsoft\Windows\CurrentVersion\Run" -ErrorAction SilentlyContinue).Queqiao
    [pscustomobject]@{ Entry = $entry; Run = $run }
}
function Get-AgentsThere { @(Get-Process queqiao-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$target\*" }) }

'=== 0. the window (nothing installed)'
$text = @(Invoke-Setup 'window' "--ui-picture `"$out\setup_window.bmp`"")
"--- window"; $text
Add-Check 'the installer window was drawn' (($text -contains 'RESULT PASS') -and (Test-Path -LiteralPath "$out\setup_window.bmp"))
Add-Check 'the window installed nothing' (-not (Test-Path -LiteralPath $target))

'=== 1. a quiet install, starting with Windows'
$text = @(Invoke-Setup 'install' '--quiet --autostart')
"--- install"; $text
Add-Check 'installer: RESULT PASS' ($text -contains 'RESULT PASS')
foreach ($program in 'queqiao-viewer.exe', 'queqiao-agent.exe', 'queqiao-service.exe', 'queqiao-setup.exe') {
    $file = Join-Path $target $program
    $signature = if (Test-Path -LiteralPath $file) { Get-AuthenticodeSignature -LiteralPath $file } else { $null }
    Add-Check "installed and signed: $program" ($signature -and $signature.SignerCertificate.Thumbprint -eq $certificate.Thumbprint)
}
$installed = Get-Installed
Add-Check 'Settings > Apps entry with an uninstall command' ($installed.Entry -and $installed.Entry.UninstallString -like "*queqiao-setup.exe*--uninstall*")
Add-Check 'Start menu shortcut to the viewer' ((Test-Path -LiteralPath $shortcut) -and ((New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath -like '*queqiao-viewer.exe'))
Add-Check 'starts with Windows (Run entry to the agent)' ($installed.Run -like "*$target\queqiao-agent.exe*")
# It stays (in the tray) even when it cannot go online: give it 5 s.
Start-Sleep -Seconds 5
$first = Get-AgentsThere
Add-Check 'the agent runs from the installation (after 5 s)' ($first.Count -ge 1)

'=== 2. the same again: an update'
$text = @(Invoke-Setup 'update' '--quiet --autostart')
"--- update"; $text
Add-Check 'update: RESULT PASS' ($text -contains 'RESULT PASS')
Add-Check 'update: the earlier installation replaced' (@($text | Where-Object { $_ -eq 'the earlier installation was replaced' }).Count -eq 1)
Add-Check 'update: the running agent was ended first' ($first.Count -ge 1 -and @($text | Where-Object { $_ -like 'ended *queqiao-agent.exe*' }).Count -ge 1)
Start-Sleep -Seconds 3
Add-Check 'update: the new agent runs' ((Get-AgentsThere).Count -ge 1)
Add-Check 'update: no copy of the earlier installation left' (-not (Test-Path -LiteralPath ($target + '.old')))

'=== 3. a quiet uninstall'
$text = @(Invoke-Setup 'uninstall' '--uninstall --quiet')
"--- uninstall"; $text
Add-Check 'uninstall: the running agent was ended' (@($text | Where-Object { $_ -like 'ended *queqiao-agent.exe*' }).Count -ge 1)
Add-Check 'uninstall: RESULT PASS' ($text -contains 'RESULT PASS')
Start-Sleep -Seconds 5
$installed = Get-Installed
Add-Check 'uninstall: the programs folder is gone' (-not (Test-Path -LiteralPath $target))
Add-Check 'uninstall: the Apps entry is gone' (-not $installed.Entry)
Add-Check 'uninstall: the shortcut is gone' (-not (Test-Path -LiteralPath $shortcut))
Add-Check 'uninstall: the Run entry is gone' (-not $installed.Run)
Add-Check 'uninstall: no agent runs from there' ((Get-AgentsThere).Count -eq 0)

'=== 4. with the service, then uninstall'
$programFiles = Join-Path $env:ProgramFiles 'Queqiao'
$text = @(Invoke-Setup 'service' '--quiet --service')
"--- service"; $text
Add-Check 'install with the service: RESULT PASS' ($text -contains 'RESULT PASS')
$service = Get-Service Queqiao -ErrorAction SilentlyContinue
Add-Check 'the Queqiao service runs' ($service -and $service.Status -eq 'Running')
Add-Check 'its programs are in Program Files' (Test-Path -LiteralPath (Join-Path $programFiles 'queqiao-service.exe'))
$text = @(Invoke-Setup 'service-uninstall' '--uninstall --quiet')
"--- service-uninstall"; $text
Add-Check 'uninstall with the service: RESULT PASS' ($text -contains 'RESULT PASS')
Start-Sleep -Seconds 5
Add-Check 'the service is gone' (-not (Get-Service Queqiao -ErrorAction SilentlyContinue))
Add-Check 'Program Files\Queqiao is gone' (-not (Test-Path -LiteralPath $programFiles))
Add-Check 'the programs folder is gone again' (-not (Test-Path -LiteralPath $target))

if ($failed.Count) { "M7_SETUP_FAIL $($failed -join ', ')" } else { 'M7_SETUP_PASS' }
