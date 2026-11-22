# M7 check of updates on one PC (run as an administrator; the installer and the agent run in the signed-in person's
# session). Version 0.1.0 is target\release; this script builds the same code as 0.1.1 (QUEQIAO_VERSION) into
# target\v011, packages both (scripts\package.ps1), and serves an update feed on 127.0.0.1 (the person's registry
# points the agent at it: UpdateFeed, UpdateFirstCheckSeconds). Then
# 1. 0.1.0 installed: the agent finds 0.1.1 (signed with the release key), downloads it, starts its installer with
#    --update and leaves; 0.1.1 is installed and its agent runs;
# 2. the feed offers 0.1.2 whose agent cannot start (a text file): the installer puts 0.1.1 back and starts it, and
#    0.1.1 does not try 0.1.2 again;
# 3. uninstalled, the test registry values removed.
# Needs the release key (queqiao-setup release-key) in -Key. Prints M7_UPDATE_PASS or M7_UPDATE_FAIL. ASCII only.
param(
    [string]$Key = (Join-Path $env:USERPROFILE '.queqiao-release\release_key.bin'),
    [int]$Port = 47140,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\probe\m7_update'
if (Test-Path -LiteralPath $out) { Remove-Item -Recurse -Force -LiteralPath $out }
$feed = Join-Path $out 'feed'
New-Item -ItemType Directory -Force $feed | Out-Null
$consoleUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $consoleUser) { 'RUN_FAIL nobody is signed in at the console'; return }
if (-not (Test-Path -LiteralPath $Key)) { "RUN_FAIL no release key at $Key"; return }
$failed = @()
function Add-Check([string]$Name, [bool]$Ok) {
    '{0,-58} {1}' -f $Name, $(if ($Ok) { 'ok' } else { 'FAIL' })
    if (-not $Ok) { $script:failed += $Name }
}

# The person's folders and registry, as the installer and the agent see them in their session.
$sid = (New-Object Security.Principal.NTAccount $consoleUser).Translate([Security.Principal.SecurityIdentifier]).Value
$userHome = (Get-CimInstance Win32_UserProfile | Where-Object { $_.SID -eq $sid }).LocalPath
$target = Join-Path $userHome 'AppData\Local\Programs\Queqiao'
$updates = Join-Path $userHome 'AppData\Local\Queqiao\updates'
$userKey = "Registry::HKEY_USERS\$sid\Software\Queqiao"
$uninstallKey = "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows\CurrentVersion\Uninstall\Queqiao"

'=== builds'
$v1 = Join-Path $root 'target\release'
$v2 = Join-Path $root 'target\v011\release'
if (-not $SkipBuild) {
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $vsRoot = if (Test-Path -LiteralPath $vswhere) { & $vswhere -latest -products * -property installationPath } else { $null }
        if ($vsRoot) { $env:Path = (Join-Path $vsRoot 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin') + ";$env:Path" }
    }
    $env:QUEQIAO_VERSION = '0.1.1'
    # A test build (cargo build-test, .cargo\config.toml), as target\release: a shipping 0.1.1 would ignore the test feed.
    $build = & "$env:USERPROFILE\.cargo\bin\cargo.exe" build-test --release --target-dir (Join-Path $root 'target\v011') -p queqiao-agent -p queqiao-viewer -p queqiao-service -p queqiao-setup 2>&1 | ForEach-Object { "$_" }
    $buildExit = $LASTEXITCODE
    Remove-Item Env:\QUEQIAO_VERSION
    $build | Select-Object -Last 2
    if ($buildExit) { 'M7_UPDATE_FAIL building 0.1.1'; return }
}
$installers = @{}
foreach ($bin in $v1, $v2) {
    $package = @(& (Join-Path $PSScriptRoot 'package.ps1') -Bin $bin)
    if ($package[-1] -notlike 'PACKAGE_PASS*') { $package; 'M7_UPDATE_FAIL packaging'; return }
    $file = ($package[-1] -split ' ')[1]
    $installers[[IO.Path]::GetFileNameWithoutExtension($file).Replace('queqiao-setup-', '')] = $file
}
"installers: $(($installers.Keys | Sort-Object) -join ', ')"
if (-not ($installers['0.1.0'] -and $installers['0.1.1'])) { 'M7_UPDATE_FAIL expected 0.1.0 and 0.1.1'; return }
function Get-Version([string]$Program) {
    if (-not (Test-Path -LiteralPath $Program)) { return 'none' }
    ((& $Program --version 2>$null | Select-Object -First 1) -split ' ')[1]
}

function Publish([string]$Installer, [string]$Version, [string]$Notes) {
    @(& (Join-Path $v1 'queqiao-setup.exe') release --key $Key --installer $Installer --version $Version --output $feed --notes $Notes 2>&1 | ForEach-Object { "$_" })
}
$published = Publish $installers['0.1.1'] '0.1.1' 'Test update'
$published
Add-Check 'the feed is signed with the build key' ([bool]($published -match ": this build's release key"))

# The feed on 127.0.0.1 (a job of this script: it ends with it).
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

function Invoke-AsUser([string]$Name, [string]$Program, [string]$Arguments, [string]$Report, [int]$TimeoutSec = 120) {
    $action = New-ScheduledTaskAction -Execute $Program -Argument $Arguments
    $principal = New-ScheduledTaskPrincipal -UserId $consoleUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSec)
    Register-ScheduledTask -TaskName "Queqiao-M7-$Name" -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName "Queqiao-M7-$Name"
    if ($Report) {
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        while (-not (Test-Path -LiteralPath $Report) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
    } else {
        Start-Sleep -Seconds 2
    }
    Start-Sleep -Milliseconds 500
    Unregister-ScheduledTask -TaskName "Queqiao-M7-$Name" -Confirm:$false
}
function Wait-UpdateReport([datetime]$After, [int]$TimeoutSec) {
    $reportFile = Join-Path $updates 'update_report.txt'
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path -LiteralPath $reportFile) -and (Get-Item -LiteralPath $reportFile).LastWriteTime -gt $After) {
            $text = @(Get-Content -LiteralPath $reportFile)
            if ($text -match '^RESULT ') { return $text }
        }
        Start-Sleep -Milliseconds 500
    }
    @('no update report')
}
function Get-AgentsThere { @(Get-Process queqiao-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$target\*" }) }

try {
    '=== 1. 0.1.0 installed, 0.1.1 on the feed'
    $installReport = Join-Path $out 'install.txt'
    $started = Get-Date
    Invoke-AsUser 'install' $installers['0.1.0'] "--quiet --autostart --report `"$installReport`"" $installReport
    Get-Content -LiteralPath $installReport -ErrorAction SilentlyContinue | Where-Object { $_ -match 'RESULT|ERROR|agent|sign in' }
    # Its agent looks at the feed 20 s after it started.
    Add-Check 'installed 0.1.0, its agent running' (((Get-Version (Join-Path $target 'queqiao-agent.exe')) -eq '0.1.0') -and (Get-AgentsThere).Count -ge 1)
    $text = Wait-UpdateReport $started 120
    '--- update report'; $text
    Start-Sleep -Seconds 3
    $asked = @(Get-Content -LiteralPath $requests -ErrorAction SilentlyContinue)
    Add-Check 'the agent read the manifest, signature and installer' ((@($asked) -contains 'queqiao-update.json') -and (@($asked) -contains 'queqiao-update.json.sig') -and (@($asked) -contains 'queqiao-setup-0.1.1.exe'))
    Add-Check 'update: RESULT PASS' ($text -contains 'RESULT PASS')
    Add-Check 'update: start with Windows kept' (@($text | Where-Object { $_ -like 'update to 0.1.1 (start with Windows: true)' }).Count -eq 1)
    Add-Check 'update: the earlier installation replaced' ($text -contains 'the earlier installation was replaced')
    Add-Check 'the installed agent is 0.1.1' ((Get-Version (Join-Path $target 'queqiao-agent.exe')) -eq '0.1.1')
    Add-Check 'Settings > Apps shows 0.1.1' ((Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion -eq '0.1.1')
    Add-Check 'an agent runs from the installation' ((Get-AgentsThere).Count -ge 1)
    Add-Check 'no copy of 0.1.0 left' (-not (Test-Path -LiteralPath ($target + '.old')))

    '=== 2. 0.1.2 on the feed, its agent cannot start'
    $broken = Join-Path $out 'broken'
    New-Item -ItemType Directory -Force $broken | Out-Null
    Set-Content -LiteralPath (Join-Path $broken 'queqiao-agent.exe') -Value 'not a program' -Encoding ascii
    $brokenInstaller = Join-Path $out 'broken-setup.exe'
    & (Join-Path $v2 'queqiao-setup.exe') pack --output $brokenInstaller (Join-Path $v2 'queqiao-viewer.exe') (Join-Path $broken 'queqiao-agent.exe') (Join-Path $v2 'queqiao-service.exe') | Out-Null
    Publish $brokenInstaller '0.1.2' 'Broken on purpose' | Out-Null
    # The running 0.1.1 looks again when it starts: start it again.
    Get-AgentsThere | Stop-Process -Force
    Start-Sleep -Seconds 1
    $started = Get-Date
    # Through cmd's start: the agent is not the task's own process (that may end with the task).
    Invoke-AsUser 'agent' 'cmd.exe' ('/c start "" "' + (Join-Path $target 'queqiao-agent.exe') + '"') ''
    $text = Wait-UpdateReport $started 120
    '--- update report'; $text
    Add-Check 'rollback: RESULT FAIL (the update did not hold)' ($text -contains 'RESULT FAIL')
    Add-Check 'rollback: the new agent did not keep running' (@($text | Where-Object { $_ -like 'the new agent did not keep running*' }).Count -eq 1)
    Add-Check 'rollback: the earlier version is back' ($text -contains 'the earlier version is back')
    Start-Sleep -Seconds 2
    Add-Check 'rollback: the installed agent is 0.1.1 again' ((Get-Version (Join-Path $target 'queqiao-agent.exe')) -eq '0.1.1')
    Add-Check 'rollback: Settings > Apps still shows 0.1.1' ((Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue).DisplayVersion -eq '0.1.1')
    Add-Check 'rollback: the 0.1.1 agent runs' ((Get-AgentsThere).Count -ge 1)
    Add-Check 'rollback: no copy left over' (-not (Test-Path -LiteralPath ($target + '.old')))
    # The agent that came back looks again 20 s after it started: it must not try 0.1.2 again.
    $reportTime = (Get-Item -LiteralPath (Join-Path $updates 'update_report.txt')).LastWriteTime
    Start-Sleep -Seconds 30
    $asked = @(Get-Content -LiteralPath $requests)
    $lastDownload = [Array]::LastIndexOf($asked, 'queqiao-setup-0.1.2.exe')
    $since = @(if ($lastDownload -ge 0) { $asked | Select-Object -Skip ($lastDownload + 1) })
    Add-Check 'the agent that came back looked at the feed' ($lastDownload -ge 0 -and $since -contains 'queqiao-update.json')
    Add-Check 'and did not download 0.1.2 again' (@($asked | Where-Object { $_ -eq 'queqiao-setup-0.1.2.exe' }).Count -eq 1)
    Add-Check 'nor install it again' ((Get-Item -LiteralPath (Join-Path $updates 'update_report.txt')).LastWriteTime -eq $reportTime)
    Add-Check 'the 0.1.1 agent still runs' ((Get-AgentsThere).Count -ge 1)
} finally {
    '=== 3. uninstall'
    $uninstallReport = Join-Path $out 'uninstall.txt'
    Invoke-AsUser 'uninstall' (Join-Path $target 'queqiao-setup.exe') "--uninstall --quiet --report `"$uninstallReport`"" $uninstallReport
    Start-Sleep -Seconds 4
    Add-Check 'uninstall: RESULT PASS' (@(Get-Content -LiteralPath $uninstallReport -ErrorAction SilentlyContinue) -contains 'RESULT PASS')
    Add-Check 'uninstall: the programs and downloaded updates are gone' (-not (Test-Path -LiteralPath $target) -and -not (Test-Path -LiteralPath $updates))
    foreach ($valueName in 'UpdateFeed', 'UpdateFirstCheckSeconds', 'UpdateTried') { Remove-ItemProperty -LiteralPath $userKey -Name $valueName -ErrorAction SilentlyContinue }
    Stop-Job $server -ErrorAction SilentlyContinue
    Remove-Job $server -Force -ErrorAction SilentlyContinue
}
if ($failed.Count) { "M7_UPDATE_FAIL $($failed -join ', ')" } else { 'M7_UPDATE_PASS' }
