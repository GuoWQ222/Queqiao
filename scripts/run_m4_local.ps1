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
    # Which sessions to run: all, or some of direct, relay, allowed, declined, ended-here.
    [string[]]$Sessions = @('all'),
    # Sound: the agent captures -AgentSoundDevice and the viewer plays into -ViewerSoundDevice (parts of the
    # device names; use two virtual devices, so nothing is heard and the viewer's sound is not captured again).
    # In the direct and relay sessions the agent plays a test tone there and the viewer must hear it. Without them the
    # viewer starts muted: on one PC it would otherwise play the agent's sound back into what the agent captures.
    [string]$AgentSoundDevice = '',
    [string]$ViewerSoundDevice = '',
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
    # Files for the direct session, one each way (random content, so a mix-up cannot pass).
    $fileToAgent = Join-Path $Out 'queqiao-test-to-agent.bin'
    $fileToViewer = Join-Path $Out 'queqiao-test-to-viewer.bin'
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    foreach ($testFile in $fileToAgent, $fileToViewer) {
        $bytes = New-Object byte[] (20MB + 12345)
        $random.GetBytes($bytes)
        [IO.File]::WriteAllBytes($testFile, $bytes)
    }
    # label, QUEQIAO_FORCE, extra agent options, what the agent's report must say, what the viewer's must say,
    # whether both runs must pass (a declined session passes neither). Later options win, so an extra --seconds shortens the agent's run.
    $runs = @(
        @('direct', '', "--clipboard-test --dry-run-commands --send-file `"$fileToViewer`"", 'test text from the other side received: yes', 'test text from the other side received: yes', $true),
        @('relay', 'network:relay', '', 'path after .*: relayed through ', 'path after .*: relayed through ', $true),
        @('allowed', '', "--ask --answer allow --ui-pictures `"$Out`"", 'the person at this PC allowed', 'RESULT PASS', $true),
        @('declined', '', '--ask --answer deny --seconds 45', 'the person at this PC declined', 'the person at that PC declined', $false),
        @('ended-here', '', '--end-after 6', 'ended: ended by the person at this PC', 'ended: the agent ended the session', $true)
    )
    foreach ($run in $runs) {
        $label, $mode, $extra, $agentExpects, $viewerExpects, $bothPass = $run
        if ($Sessions -notcontains $label -and $Sessions -notcontains 'all') { continue }
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --verify-input --report `"$Out\agent_$label.txt`" $extra"
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --device $device --seconds $Seconds --input-test --width 960 --report `"$Out\viewer_$label.txt`""
        # The clipboard and file tests run on both sides; the viewer sends commands the agent only reports.
        if ($extra -like '*--clipboard-test*') { $viewerArgs += " --clipboard-test --send-file `"$fileToAgent`"" }
        if ($extra -like '*--dry-run-commands*') { $viewerArgs += ' --command ctrl-alt-del --command lock --command restart' }
        # Full screen: the drawing area must cover the monitor (the input test maps to it too).
        if ($label -eq 'ended-here') { $viewerArgs += ' --full-screen' }
        $soundTest = $AgentSoundDevice -and $ViewerSoundDevice
        if ($soundTest) {
            $agentArgs += " --sound-device `"$AgentSoundDevice`""
            $viewerArgs += " --sound-device `"$ViewerSoundDevice`""
            if ($label -in @('direct', 'relay')) {
                $agentArgs += ' --sound-test'
                $viewerArgs += ' --sound-test'
            }
        } else {
            $viewerArgs += ' --mute'
        }
        Start-SessionTask "Queqiao-M4-Agent-$label" $agent $agentArgs $mode
        Start-Sleep -Seconds 2
        Start-SessionTask "Queqiao-M4-Viewer-$label" $viewer $viewerArgs $mode
        $viewerResult = Wait-SessionTask "Queqiao-M4-Viewer-$label" ($Seconds + 120)
        $agentResult = Wait-SessionTask "Queqiao-M4-Agent-$label" 60
        "=== session '$label' (QUEQIAO_FORCE=$mode $extra): agent exit $agentResult, viewer exit $viewerResult"
        foreach ($check in @(@('agent', $agentExpects, $bothPass), @('viewer', $viewerExpects, $bothPass))) {
            $program, $expects, $mustPass = $check
            $report = Join-Path $Out "${program}_$label.txt"
            $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
            "--- $program"
            $text
            if ($mustPass -and $text[-1] -ne 'RESULT PASS') { $failed += "$label $program" }
            if (-not ($text | Where-Object { $_ -match $expects })) { $failed += "$label $program (expected '$expects')" }
            if ($soundTest -and ($label -in @('direct', 'relay')) -and $program -eq 'viewer' -and -not ($text | Where-Object { $_ -match '^sound test: PASS' })) {
                $failed += "$label viewer (expected 'sound test: PASS')"
            }
            if ($label -eq 'ended-here' -and $program -eq 'viewer' -and -not ($text | Where-Object { $_ -match '^window: full screen, drawing area (\d+x\d+), monitor \1$' })) {
                $failed += "$label viewer (expected the full-screen window to cover the monitor)"
            }
            # The window title shows the network status (the relay session must say so).
            if ($program -eq 'viewer' -and $mustPass) {
                $pathWord = if ($mode -eq 'network:relay') { 'relayed' } else { 'direct' }
                $titlePattern = "^window title: Queqiao - .+ \| \d+ ms \| \d+ fps \| \d+(\.\d)? [kM]bit/s \| $pathWord"
                if (-not ($text | Where-Object { $_ -match $titlePattern })) { $failed += "$label viewer (expected '$titlePattern')" }
            }
            if ($extra -like '*--dry-run-commands*') {
                $wanted = if ($program -eq 'agent') {
                    @('command from the viewer: CtrlAltDel: Ctrl\+Alt\+Del needs the Queqiao service', 'command from the viewer: Lock: the PC would be locked now', 'command from the viewer: Restart: the PC would restart in \d+ s')
                } else {
                    @('notice from that PC: Ctrl\+Alt\+Del needs', 'notice from that PC: the PC would be locked now', 'notice from that PC: the PC would restart')
                }
                foreach ($pattern in $wanted) {
                    if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "$label $program (expected '$pattern')" }
                }
            }
            if ($extra -like '*--send-file*') {
                # The file from the other side must be on disk, identical; then it is removed again.
                $original = if ($program -eq 'agent') { $fileToAgent } else { $fileToViewer }
                $line = $text | Where-Object { $_ -match '^file received: (.+) \(\d+ bytes' } | Select-Object -First 1
                $received = if ($line -match '^file received: (.+) \(\d+ bytes') { $Matches[1] } else { $null }
                if ($received -and (Test-Path -LiteralPath $received)) {
                    $same = (Get-FileHash -LiteralPath $received -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash
                    "file check ($program): $received identical to the original: $same"
                    if (-not $same) { $failed += "$label $program file differs" }
                    Remove-Item -LiteralPath $received -Force
                } else {
                    $failed += "$label $program received no file"
                }
            }
        }
    }
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_err.txt" -ErrorAction SilentlyContinue | Select-Object -Last 20
}
if ($failed) { "M4_LOCAL_FAIL: $($failed -join '; ')" } else { 'M4_LOCAL_PASS' }
