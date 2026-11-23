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
    # This PC has no AV1 encoder that works (an Intel Iris Xe of the 12th generation registers one that refuses
    # everything: sunshine, 2026-11-23). The av1 session then checks the fallback: the agent says so and stays on H.264.
    # Not found out here: only the agent's own check (it opens the encoder) knows, and that is what is tested.
    [switch]$NoAv1Encoder,
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
$password1 | & $server add-user alice --data $data | Out-Null
$password2 | & $server add-user bob --data $data | Out-Null
# The help session's second account: the probe's lockout checks leave bob locked for 15 minutes.
$password3 = [guid]::NewGuid().ToString('N')
$password3 | & $server add-user carol --data $data | Out-Null
$serveArgs = @('serve', '--listen', $listen, '--data', "`"$data`"", '--public-ip', $Address, '--relay-private', '--allow-signup')
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
        $probe = & (Join-Path $Bin 'examples\account_probe.exe') $listen $pin alice $password1 bob $password2 --signup-open
        $probe
        if ($probe[-1] -ne 'PROBE_PASS') { $failed += 'probe' }
        # The probe fails logins and codes on purpose (lockout checks); the server starts again so its in-memory
        # failure counts do not lock the sessions out. Accounts and devices stay (they are in its database).
        Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
        $serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out2.txt" -RedirectStandardError "$Out\server_err.txt"
        $deadline = (Get-Date).AddSeconds(10)
        do {
            Start-Sleep -Milliseconds 200
            $client = New-Object Net.Sockets.TcpClient
            try { $client.Connect($Address, 47100); $up = $true } catch { $up = $false } finally { $client.Dispose() }
        } while (-not $up -and (Get-Date) -lt $deadline)
        if (-not $up) { 'RUN_FAIL the server did not start again'; return }
        # Sign-up and the account hardening (lockouts on purpose too), against the server just started: the checks
        # above used up this address's tries.
        $accounts = @(& (Join-Path $Bin 'examples\account_probe.exe') $listen $pin alice $password1 bob $password2 --signup-open --accounts)
        $accounts
        if ($accounts[-1] -ne 'PROBE_PASS') { $failed += 'probe accounts' }
        Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
        $serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out2b.txt" -RedirectStandardError "$Out\server_err2b.txt"
        $deadline = (Get-Date).AddSeconds(10)
        do {
            Start-Sleep -Milliseconds 200
            $client = New-Object Net.Sockets.TcpClient
            try { $client.Connect($Address, 47100); $up = $true } catch { $up = $false } finally { $client.Dispose() }
        } while (-not $up -and (Get-Date) -lt $deadline)
        if (-not $up) { 'RUN_FAIL the server did not start for the burst'; return }
        # Ten wrong logins at once, against the server just started (nothing counted for this address yet).
        $burst = @(& (Join-Path $Bin 'examples\account_probe.exe') $listen $pin --burst)
        $burst
        if ($burst[-1] -ne 'PROBE_PASS') { $failed += 'probe burst' }
        # The burst ends with this address locked out (a saved login still gets in, a password does not): once more
        # anew, for the sessions' password logins.
        Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
        $serverProcess = Start-Process -FilePath $server -ArgumentList $serveArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out3.txt" -RedirectStandardError "$Out\server_err3.txt"
        $deadline = (Get-Date).AddSeconds(10)
        do {
            Start-Sleep -Milliseconds 200
            $client = New-Object Net.Sockets.TcpClient
            try { $client.Connect($Address, 47100); $up = $true } catch { $up = $false } finally { $client.Dispose() }
        } while (-not $up -and (Get-Date) -lt $deadline)
        if (-not $up) { 'RUN_FAIL the server did not start a third time'; return }
    }

    # Crash reports: a crash on purpose must leave a report with what panicked.
    $crashFolder = Join-Path $env:APPDATA 'Queqiao\crashes'
    foreach ($program in @($agent, $viewer)) {
        $programName = [IO.Path]::GetFileNameWithoutExtension($program)
        $started = Get-Date
        Start-Process -FilePath $program -ArgumentList '--crash-test' -Wait -WindowStyle Hidden
        $report = Get-ChildItem -LiteralPath $crashFolder -Filter "$programName-*.txt" -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -ge $started.AddSeconds(-1) } | Select-Object -First 1
        $written = $report -and ((Get-Content -LiteralPath $report.FullName -Raw) -match 'crash test \(asked for with --crash-test\)')
        "crash report ($programName): $(if ($written) { $report.Name } else { 'missing' })"
        if (-not $written) { $failed += "$programName crash report" }
        if ($report) { Remove-Item -LiteralPath $report.FullName -Force }
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
    # A folder with a file inside a subfolder, for the allowed session (sent as a whole, inside kept).
    $folderToAgent = Join-Path $Out 'queqiao-test-folder'
    Remove-Item -LiteralPath $folderToAgent -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force (Join-Path $folderToAgent 'inner') | Out-Null
    [IO.File]::WriteAllText((Join-Path $folderToAgent 'top.txt'), 'queqiao folder test')
    $bytes = New-Object byte[] 300000
    $random.GetBytes($bytes)
    [IO.File]::WriteAllBytes((Join-Path $folderToAgent 'inner\deep.bin'), $bytes)
    # A file copied to the clipboard on the viewer's side (allowed session).
    $clipFile = Join-Path $Out 'queqiao-clipboard-file.txt'
    [IO.File]::WriteAllText($clipFile, 'queqiao clipboard file test')
    # label, QUEQIAO_FORCE, extra agent options, what the agent's report must say, what the viewer's must say,
    # whether both runs must pass (a declined session passes neither). Later options win, so an extra --seconds shortens the agent's run.
    # A virtual microphone on this PC (the names the agent pairs: VB-CABLE, VoiceMeeter, Virtual Audio Cable): with one
    # the av1 session's microphone check must pass, without one the agent must tell the viewer it has none.
    $virtualMicrophone = @(Get-PnpDevice -Class AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue |
        Where-Object { $_.Status -eq 'OK' -and $_.FriendlyName -match 'CABLE Output|VoiceMeeter Out|Virtual Audio Cable' }).Count -gt 0
    $av1Agent = if ($NoAv1Encoder) { 'picture: H\.264 .*\(AV1 asked for, but this PC has no AV1 encoder\)' } else { 'command from the viewer: picture: AV1 \(' }
    $av1Viewer = if ($NoAv1Encoder) { 'notice from that PC: picture: H\.264 .*AV1 asked for, but this PC has no AV1 encoder' } else { 'the picture is AV1 now' }
    "this PC: AV1 encoder $(if ($NoAv1Encoder) { 'none (-NoAv1Encoder)' } else { 'expected' }), virtual microphone $(if ($virtualMicrophone) { 'yes' } else { 'none' })"
    $runs = @(
        @('direct', '', "--clipboard-test --dry-run-commands --send-file `"$fileToViewer`"", 'test text from the other side received: yes', 'test text from the other side received: yes', $true),
        @('relay', 'network:relay', '', 'path after .*: relayed through ', 'path after .*: relayed through ', $true),
        @('allowed', '', "--ask --answer allow --privacy-test --ui-pictures `"$Out`"", 'the person at this PC allowed', 'RESULT PASS', $true),
        @('declined', '', '--ask --answer deny --seconds 45', 'the person at this PC declined', 'the person at that PC declined', $false),
        # Through this PC's uplinks' sockets (network:uplinks: they come first even though the server is on the
        # loopback), with a file sent and cancelled: a burst there ended sessions ("DTLS error: receive queue full").
        @('ended-here', 'network:uplinks', '--end-after 6 --lock-after-session --dry-run-commands', 'ended: ended by the person at this PC', 'ended: the agent ended the session', $true),
        # AV1 asked for: the agent switches to its AV1 encoder, the viewer to Windows' AV1 decoder (needs both).
        # The viewer's microphone too (a test tone): the agent plays it into this PC's virtual microphone and hears it there.
        @('av1', '', '--mic-check', $av1Agent, $av1Viewer, $true),
        # A File Explorer window dragged round the screen for 12 s mid-session (scripts\drag_window.ps1), with the
        # bitrate course on both sides and a picture in the middle of it. The viewer asks for 4 Mbit/s at most: the
        # agent's encoder must follow while it encodes (Intel's Quick Sync kept 12-17 Mbit/s before, 2026-11-23).
        @('drag', '', '', 'bitrate course, every second', 'arrival course, every second', $true)
    )
    foreach ($run in $runs) {
        $label, $mode, $extra, $agentExpects, $viewerExpects, $bothPass = $run
        if ($Sessions -notcontains $label -and $Sessions -notcontains 'all') { continue }
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --verify-input --report `"$Out\agent_$label.txt`" $extra"
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --device $device --seconds $Seconds --input-test --width 960 --report `"$Out\viewer_$label.txt`""
        # The clipboard and file tests run on both sides; the viewer sends commands the agent only reports.
        if ($extra -like '*--clipboard-test*') { $viewerArgs += " --clipboard-test --mute-speakers --send-file `"$fileToAgent`"" }
        if ($extra -like '*--dry-run-commands*') { $viewerArgs += ' --command ctrl-alt-del --command lock --command restart' }
        # Full screen: the drawing area must cover the monitor (the input test maps to it too).
        if ($label -eq 'ended-here') { $viewerArgs += " --full-screen --picture 30,sharp --resolution 720 --send-file `"$fileToAgent`" --cancel-test" }
        # A command line on that PC, in a window of its own that closes by itself.
        if ($label -eq 'relay') { $viewerArgs += ' --terminal-test --game-mouse-test --screen 0 --opacity 60 --boss-key-test' }
        if ($label -eq 'av1') { $viewerArgs += ' --codec av1 --mic-test' }
        # H.264 asked for (Automatic picks AV1 where both PCs have it on their GPUs): the H.264 path stays tested.
        # The bar and the card of figures on, and a picture of the window with them (viewer_session.bmp).
        if ($label -eq 'direct') { $viewerArgs += " --codec h264 --figures --ui-pictures `"$Out`"" }
        if ($label -eq 'drag') {
            New-Item -ItemType Directory -Force (Join-Path $Out 'drag') | Out-Null
            $viewerArgs += " --codec h264 --figures --ui-pictures `"$Out\drag`" --picture-at 9 --seconds 22 --max-bitrate 4000"
        }
        # Privacy screen: the agent shows a test square instead of darkening the monitors.
        if ($extra -like '*--privacy-test*') { $viewerArgs += " --privacy --max-bitrate 3000 --send-file `"$folderToAgent`" --clipboard-files-test `"$clipFile`"" }
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
        if ($label -eq 'drag') {
            $dragOut = Join-Path $Out 'drag\drag.txt'
            Remove-Item -LiteralPath $dragOut -ErrorAction SilentlyContinue
            $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
            Start-SessionTask 'Queqiao-M4-Drag' $powershell "-NoProfile -ExecutionPolicy Bypass -Command `"& '$PSScriptRoot\drag_window.ps1' -Delay 4 -Seconds 12 *> '$dragOut'`"" ''
        }
        $viewerResult = Wait-SessionTask "Queqiao-M4-Viewer-$label" ($Seconds + 120)
        $agentResult = Wait-SessionTask "Queqiao-M4-Agent-$label" 60
        if ($label -eq 'drag') {
            $null = Wait-SessionTask 'Queqiao-M4-Drag' 30
            "drag: $(if (Test-Path -LiteralPath $dragOut) { (Get-Content -LiteralPath $dragOut) -join ' ' } else { 'no output' })"
        }
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
            # A cancelled transfer: the sender reports it, the receiver drops it and keeps nothing.
            if ($label -eq 'ended-here') {
                $wanted = if ($program -eq 'agent') { '^file failed: queqiao-test-to-agent\.bin: cancelled by the sender' } else { '^cancelled queqiao-test-to-agent\.bin after \d+ bytes' }
                if (-not ($text | Where-Object { $_ -match $wanted })) { $failed += "$label $program (expected '$wanted')" }
                if ($program -eq 'agent' -and ($text | Where-Object { $_ -match '^file received: .*queqiao-test-to-agent' })) { $failed += "$label agent (a cancelled file arrived)" }
                if (Get-ChildItem -LiteralPath "$env:USERPROFILE\Downloads\Queqiao" -Filter '*.queqiao-part' -ErrorAction SilentlyContinue) { $failed += "$label (a part file was left behind)" }
            }
            # Resolution: the picture changes size during the session (a new encoder; the decoder follows).
            if ($label -eq 'ended-here') {
                $wanted = if ($program -eq 'agent') { '^command from the viewer: picture size \d+x720$' } else { '^last picture size: \d+x720$' }
                if (-not ($text | Where-Object { $_ -match $wanted })) { $failed += "$label $program (expected '$wanted')" }
            }
            if ($label -eq 'ended-here' -and $program -eq 'agent' -and -not ($text | Where-Object { $_ -match '^after the session: the PC would be locked now' })) {
                $failed += "$label agent (expected the lock after the session)"
            }
            # Picture settings: at most 30 fps (the full-screen window shows itself, so frames keep coming) and
            # twice the size-based bitrate allowed.
            if ($label -eq 'ended-here') {
                if ($program -eq 'agent') {
                    $settingsLine = $text | Where-Object { $_ -match '^command from the viewer: picture: up to 30 fps, quality Sharp: up to ([\d.]+) Mbit/s$' } | Select-Object -First 1
                    $bitrateLine = $text | Where-Object { $_ -match '^bitrate: .* highest allowed ([\d.]+) Mbit/s' } | Select-Object -First 1
                    if (-not $settingsLine -or -not $bitrateLine) { $failed += "$label agent (picture settings not applied)" }
                } else {
                    $titleLine = $text | Where-Object { $_ -match '^window title: .* \| (\d+) fps \|' } | Select-Object -First 1
                    if (-not $titleLine -or [int]$Matches[1] -gt 32) { $failed += "$label viewer (expected at most 30 fps: $titleLine)" }
                }
            }
            if ($extra -like '*--privacy-test*') {
                $wanted = if ($program -eq 'agent') { @('^privacy test: PASS', '^privacy: the session banner is above the dark screen: yes', '^privacy: the local keyboard and mouse are off$', '^command from the viewer: bitrate: up to 3\.0 Mbit/s', 'highest allowed 3\.0 Mbit/s') } else { @("^notice from that PC: that PC's screen is dark") }
                foreach ($pattern in $wanted) {
                    if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "$label $program (expected '$pattern')" }
                }
                # Copied files: the agent's clipboard holds the file that came (then it is removed).
                if ($program -eq 'agent') {
                    foreach ($pattern in @('^clipboard files: 1 put on the clipboard from the other side', '^clipboard now holds: .*queqiao-clipboard-file\.txt')) {
                        if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "$label agent (expected '$pattern')" }
                    }
                    Remove-Item -LiteralPath "$env:USERPROFILE\Downloads\Queqiao\queqiao-clipboard-file.txt" -Force -ErrorAction SilentlyContinue
                }
                # The folder: both files arrive inside it (the subfolder too) and match; then they are removed.
                if ($program -eq 'agent') {
                    $received = @($text | Where-Object { $_ -match '^file received: (.+queqiao-test-folder.+) \(\d+ bytes' } | ForEach-Object { if ($_ -match '^file received: (.+) \(\d+ bytes') { $Matches[1] } })
                    $deep = $received | Where-Object { $_ -match 'queqiao-test-folder\\inner\\deep\.bin$' }
                    $same = $deep -and ((Get-FileHash -LiteralPath $deep).Hash -eq (Get-FileHash -LiteralPath (Join-Path $folderToAgent 'inner\deep.bin')).Hash)
                    "folder check: $($received.Count) files received, inner file identical: $same"
                    if ($received.Count -ne 2 -or -not $same) { $failed += "$label agent (the folder did not arrive whole)" }
                    $receivedFolder = $received | ForEach-Object { $_ -replace '\\(top\.txt|inner\\deep\.bin)$', '' } | Select-Object -First 1
                    if ($receivedFolder -and $receivedFolder -like '*queqiao-test-folder*') { Remove-Item -LiteralPath $receivedFolder -Recurse -Force -ErrorAction SilentlyContinue }
                }
            }
            # The pointer's shape: the agent sends it when it changes (the input test moves the pointer), the viewer
            # gets every one.
            if ($label -eq 'direct' -and $program -eq 'viewer') {
                $agentReport = Join-Path $Out "agent_$label.txt"
                $sent = @(Get-Content -LiteralPath $agentReport -ErrorAction SilentlyContinue | Where-Object { $_ -match '^pointer: (\d+) shapes sent' } | ForEach-Object { [int]($_ -replace '^pointer: (\d+) shapes sent.*$', '$1') })
                $got = @($text | Where-Object { $_ -match '^pointer: (\d+) shapes from that PC' } | ForEach-Object { [int]($_ -replace '^pointer: (\d+) shapes from that PC.*$', '$1') })
                if (-not ($sent.Count -eq 1 -and $got.Count -eq 1 -and $got[0] -ge 1 -and $got[0] -eq $sent[0])) {
                    $failed += "$label viewer (expected the agent's pointer shapes, all of them: sent $sent, got $got)"
                }
            }
            if ($label -eq 'av1' -and $program -eq 'agent') {
                $micExpected = if ($virtualMicrophone) { '^microphone check: PASS' } else { 'microphone: that PC has no virtual microphone' }
                if (-not ($text | Where-Object { $_ -match $micExpected })) { $failed += "$label agent (expected '$micExpected')" }
            }
            if ($label -eq 'relay' -and $program -eq 'viewer' -and -not ($text | Where-Object { $_ -match '^boss key test: PASS' })) {
                $failed += "$label viewer (expected 'boss key test: PASS')"
            }
            if ($label -eq 'relay' -and $program -eq 'viewer' -and -not ($text | Where-Object { $_ -match '^window: opacity 60 % asked, 60 % as Windows has it$' })) {
                $failed += "$label viewer (expected the window at 60 % opacity)"
            }
            if ($label -eq 'relay' -and $program -eq 'viewer' -and -not ($text | Where-Object { $_ -match '^terminal test: PASS' })) {
                $failed += "$label viewer (expected 'terminal test: PASS')"
            }
            # Screens: the viewer learns them, and switching (here to the screen shown already) opens it anew.
            if ($label -eq 'relay') {
                $wanted = if ($program -eq 'agent') { '^command from the viewer: screen 1 \(\d+x\d+\), picture \d+x\d+$' } else { '^screens of that PC: Screen 1 \(\d+x\d+\)' }
                if (-not ($text | Where-Object { $_ -match $wanted })) { $failed += "$label $program (expected '$wanted')" }
            }
            if ($label -eq 'relay' -and $program -eq 'agent' -and -not ($text | Where-Object { $_ -match 'relative moves right 4/4' })) {
                $failed += "$label agent (expected 'relative moves right 4/4')"
            }
            # The window title shows the network status (the relay session must say so).
            if ($program -eq 'viewer' -and $mustPass) {
                $pathWord = if ($mode -eq 'network:relay') { 'relayed' } else { 'direct' }
                $titlePattern = "^window title: Queqiao - .+ \| \d+ ms \| \d+ fps \| \d+(\.\d)? [kM]bit/s \| $pathWord"
                if (-not ($text | Where-Object { $_ -match $titlePattern })) { $failed += "$label viewer (expected '$titlePattern')" }
            }
            # Input and control messages are not held up by files: the direct session moves a 21 MB file each way
            # and clipboard images in its first seconds. One every half second, the round trips after the first two
            # (the session starting) stay short (1 MB in flight per stream: 285 ms median, 2026-11-23); both sides
            # measured the path's round trip (ICE's checks), which the flow control needs.
            if ($label -eq 'direct' -and $mustPass) {
                if ($program -eq 'viewer') {
                    $course = $text | Where-Object { $_ -match '^network round trips, first minute, ms: (.+)$' } | Select-Object -First 1
                    if ($course -match '^network round trips, first minute, ms: (.+)$') {
                        $samples = @($Matches[1].Trim() -split ' ' | Select-Object -Skip 2 | ForEach-Object { [double]$_ } | Sort-Object)
                        $middle = if ($samples.Count) { $samples[[int][Math]::Floor($samples.Count / 2)] } else { $null }
                        if ($null -eq $middle -or $middle -gt 50) { $failed += "$label viewer (round trips while files move, median $middle ms > 50 ms: $($Matches[1]))" }
                    } else {
                        $failed += "$label viewer (expected 'network round trips, first minute')"
                    }
                }
                if (-not ($text | Where-Object { $_ -match '^\d+ ms: path round trip \(STUN\), shortest so far: [\d.]+ ms' })) { $failed += "$label $program (expected 'path round trip (STUN)')" }
                # Parity for the video (FEC): the viewer said it uses it, the agent sends it, the viewer gets it.
                $fecPattern = if ($program -eq 'agent') { '^video parity \(FEC\): on' } else { 'parity: [1-9]\d* came' }
                if (-not ($text | Where-Object { $_ -match $fecPattern })) { $failed += "$label $program (expected '$fecPattern')" }
            }
            # The drag session: what the agent sent while the window moved (seconds 6 to 15) stays near the 4 Mbit/s
            # the viewer asked for (the peak is 1.5 times the mean: 6 Mbit/s and some room).
            if ($label -eq 'drag' -and $program -eq 'agent') {
                $course = $text | Where-Object { $_ -match '^bitrate course, every second .*?: (.+)$' } | Select-Object -First 1
                if ($course -match '^bitrate course, every second .*?: (.+)$') {
                    $sent = @($Matches[1].Trim() -split ' ' | Select-Object -Skip 5 -First 10 | ForEach-Object { if ($_ -match '^[\d.]+/[\d.]+/([\d.]+)@') { [double]$Matches[1] } } | Sort-Object)
                    $middle = if ($sent.Count) { $sent[[int][Math]::Floor($sent.Count / 2)] } else { $null }
                    if ($null -eq $middle -or $middle -gt 7) { $failed += "drag agent (sent while dragging, median $middle Mbit/s with 4 asked for: the encoder did not follow)" }
                } else {
                    $failed += "drag agent (expected 'bitrate course')"
                }
            }
            # The session window's bar and card (the direct session has them on): pictures taken, "Figures" on the bar
            # clicked twice (the card off, then on).
            if ($label -eq 'direct' -and $program -eq 'viewer') {
                foreach ($wanted in '^session picture: .+viewer_session\.bmp$', '^session picture: .+viewer_session_tucked\.bmp$', '^card of figures: off$', '^card of figures: on$') {
                    if (-not ($text | Where-Object { $_ -match $wanted })) { $failed += "$label viewer (expected '$wanted')" }
                }
            }
            if ($extra -like '*--clipboard-test*' -and -not ($text | Where-Object { $_ -match 'test image from the other side received: yes' })) {
                $failed += "$label $program (expected the other side's test image)"
            }
            # No echo: on one PC both sides share the clipboard, so an image may come back once, not over and over.
            $imageLine = $text | Where-Object { $_ -match '^clipboard images: (\d+) received' } | Select-Object -First 1
            if ($imageLine -and $imageLine -match '^clipboard images: (\d+) received' -and [int]$Matches[1] -gt 3) {
                $failed += "$label $program (clipboard images echo: $imageLine)"
            }
            # Muted speakers: the tone must still come through (the sound test), and the mute is put back.
            if ($extra -like '*--clipboard-test*' -and $soundTest -and $program -eq 'agent') {
                foreach ($pattern in @("^command from the viewer: that PC's speakers are muted", '^speakers: put back \(muted: False\)$')) {
                    if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "$label agent (expected '$pattern')" }
                }
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
    # Helping another account: carol's agent (a throwaway device) takes help requests with a code; alice's viewer
    # gives its help ID and code. The agent must ask (answered here), and the session runs with fewer rights.
    if ($Sessions -contains 'help' -or $Sessions -contains 'all') {
        $idFile = Join-Path $Out 'help_id.txt'
        Remove-Item -LiteralPath $idFile -ErrorAction SilentlyContinue
        $helpCode = 'M4HELPCODE'
        $agentArgs = "--server $listen --pin $pin --user carol --password $password3 --test-device m4-helped --sessions 1 --seconds $($Seconds + 90) --verify-input --dry-run-commands --test-press-setting --answer allow --help-mode ask --help-code $helpCode --help-id-file `"$idFile`" --report `"$Out\agent_help.txt`""
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --seconds $Seconds --input-test --width 960 --report `"$Out\viewer_help.txt`""
        if ($AgentSoundDevice -and $ViewerSoundDevice) {
            $agentArgs += " --sound-device `"$AgentSoundDevice`""
            $viewerArgs += " --sound-device `"$ViewerSoundDevice`""
        } else {
            $viewerArgs += ' --mute'
        }
        Start-SessionTask 'Queqiao-M4-Agent-help' $agent $agentArgs ''
        $until = (Get-Date).AddSeconds(30)
        while (-not (Test-Path -LiteralPath $idFile) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 300 }
        $helpId = if (Test-Path -LiteralPath $idFile) { (Get-Content -LiteralPath $idFile -Raw).Trim() } else { '' }
        if (-not $helpId) { $failed += 'help agent (no help ID within 30 s)' }
        # What a helper may not do: restart, Ctrl+Alt+Del, darken the screen, command lines, sending files.
        $viewerArgs += " --command restart --command ctrl-alt-del --privacy --terminal-test --send-file `"$fileToAgent`""
        # The viewer asks without the code: the person at the helped PC allows it (--answer allow).
        Start-SessionTask 'Queqiao-M4-Viewer-help' $viewer "$viewerArgs --help-id $helpId" ''
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-help' ($Seconds + 120)
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-help' 60
        "=== session 'help' (help ID $helpId): agent exit $agentResult, viewer exit $viewerResult"
        $helpChecks = @(
            @('agent', @('^help: open, help ID \d{9}$', '^help: .* of account alice asked without a code: allowed$', '^the person at this PC allowed ', '^helper session: .* of account alice; ', '^command from the helper refused: ', '^a setting was chosen while someone of another account controls this PC: refused')),
            @('viewer', @('^help granted: ', '^input events sent: [1-9]', '^notice from that PC: someone helping from another account cannot do that', '^command line \d+ failed: the person at that PC does not allow remote command lines', '^file failed: .*receiving files is turned off on that PC'))
        )
        foreach ($check in $helpChecks) {
            $program, $patterns = $check
            $report = Join-Path $Out "${program}_help.txt"
            $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
            "--- $program"
            $text
            if ($text[-1] -ne 'RESULT PASS') { $failed += "help $program" }
            foreach ($pattern in $patterns) {
                if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "help $program (expected '$pattern')" }
            }
        }
    }
    # The same through an invitation link: the help form filled in from it and Connect pressed (the code is right, the
    # person helped is still asked).
    if ($Sessions -contains 'help-link' -or $Sessions -contains 'all') {
        $idFile = Join-Path $Out 'help_link_id.txt'
        Remove-Item -LiteralPath $idFile, (Join-Path $Out 'help_form.bmp') -ErrorAction SilentlyContinue
        $helpCode = 'M4LINKCODE'
        $agentArgs = "--server $listen --pin $pin --user carol --password $password3 --test-device m4-helped --sessions 1 --seconds $($Seconds + 90) --verify-input --answer allow --help-code $helpCode --help-id-file `"$idFile`" --report `"$Out\agent_help_link.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-help-link' $agent $agentArgs ''
        $until = (Get-Date).AddSeconds(30)
        while (-not (Test-Path -LiteralPath $idFile) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 300 }
        $helpId = if (Test-Path -LiteralPath $idFile) { (Get-Content -LiteralPath $idFile -Raw).Trim() } else { '' }
        if (-not $helpId) { $failed += 'help-link agent (no help ID within 30 s)' }
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --seconds $Seconds --input-test --width 960 --mute --invite queqiao://help/$helpId/$helpCode --test-submit-help --report `"$Out\viewer_help_link.txt`""
        Start-SessionTask 'Queqiao-M4-Viewer-help-link' $viewer $viewerArgs ''
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-help-link' ($Seconds + 120)
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-help-link' 60
        "=== session 'help-link' (help ID $helpId): agent exit $agentResult, viewer exit $viewerResult"
        $linkChecks = @(
            @('agent', @('^help: .* of account alice was let in \(code given or ask allowed\)$', '^the person at this PC allowed ', '^helper session: ')),
            @('viewer', @('^help: from the invitation link, help ID \d{9}$', '^help granted: ', '^input events sent: [1-9]'))
        )
        foreach ($check in $linkChecks) {
            $program, $patterns = $check
            $report = Join-Path $Out "${program}_help_link.txt"
            $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
            "--- $program"
            $text
            if ($text[-1] -ne 'RESULT PASS') { $failed += "help-link $program" }
            foreach ($pattern in $patterns) {
                if (-not ($text | Where-Object { $_ -match $pattern })) { $failed += "help-link $program (expected '$pattern')" }
            }
        }
        if (-not (Test-Path -LiteralPath (Join-Path $Out 'help_form.bmp'))) { $failed += 'help-link (no picture of the help form)' }
    }
    # A fixed help code: the agent keeps help open with it; two helpers in turn use the same help ID and code (the code
    # takes requests again after the first one).
    if ($Sessions -contains 'help-fixed' -or $Sessions -contains 'all') {
        $idFile = Join-Path $Out 'help_fixed_id.txt'
        Remove-Item -LiteralPath $idFile -ErrorAction SilentlyContinue
        $fixedCode = 'M4FIXEDCODE'
        $agentArgs = "--server $listen --pin $pin --user carol --password $password3 --test-device m4-helped --sessions 2 --seconds $(2 * $Seconds + 150) --verify-input --help-mode code --fixed-help-code $fixedCode --help-id-file `"$idFile`" --report `"$Out\agent_help_fixed.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-help-fixed' $agent $agentArgs ''
        $until = (Get-Date).AddSeconds(30)
        while (-not (Test-Path -LiteralPath $idFile) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 300 }
        $helpId = if (Test-Path -LiteralPath $idFile) { (Get-Content -LiteralPath $idFile -Raw).Trim() } else { '' }
        if (-not $helpId) { $failed += 'help-fixed agent (no help ID within 30 s)' }
        foreach ($turn in 1, 2) {
            $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --seconds $Seconds --input-test --width 960 --mute --help-id $helpId --help-code $fixedCode --report `"$Out\viewer_help_fixed_$turn.txt`""
            Start-SessionTask "Queqiao-M4-Viewer-help-fixed-$turn" $viewer $viewerArgs ''
            $viewerResult = Wait-SessionTask "Queqiao-M4-Viewer-help-fixed-$turn" ($Seconds + 120)
            "=== session 'help-fixed' turn $turn (help ID $helpId): viewer exit $viewerResult"
            $report = Join-Path $Out "viewer_help_fixed_$turn.txt"
            $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
            $text | Where-Object { $_ -match '^(help|RESULT|ERROR)' }
            if ($text[-1] -ne 'RESULT PASS' -or -not ($text | Where-Object { $_ -match '^help granted: ' })) { $failed += "help-fixed viewer $turn" }
        }
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-help-fixed' 60
        $report = Join-Path $Out 'agent_help_fixed.txt'
        $text = if (Test-Path -LiteralPath $report) { @(Get-Content -LiteralPath $report -Encoding UTF8) } else { @("no report at $report") }
        "--- agent (exit $agentResult)"
        $text | Where-Object { $_ -match '^(help|session|RESULT|ERROR)' }
        if ($text[-1] -ne 'RESULT PASS') { $failed += 'help-fixed agent' }
        if (@($text | Where-Object { $_ -match 'of account alice was let in \(code given or ask allowed\)$' }).Count -ne 2) { $failed += 'help-fixed agent (expected two helpers let in)' }
        if (-not ($text | Where-Object { $_ -eq 'help: open again with the fixed code' })) { $failed += 'help-fixed agent (the fixed code did not take requests again)' }
        # The code alone (as UU's "code only"): no prompt (none is answered here), no "allowed" line.
        if ($text | Where-Object { $_ -match '^the person at this PC allowed ' }) { $failed += 'help-fixed agent (asked although the code alone lets in)' }
    }
    # The default way in (the code, then the person allows it): a request without the code is declined at once, with no
    # prompt (none is answered here).
    if ($Sessions -contains 'help-noask' -or $Sessions -contains 'all') {
        $idFile = Join-Path $Out 'help_noask_id.txt'
        Remove-Item -LiteralPath $idFile -ErrorAction SilentlyContinue
        $agentArgs = "--server $listen --pin $pin --user carol --password $password3 --test-device m4-helped --sessions 1 --seconds 40 --help-code M4NOASKCODE --help-id-file `"$idFile`" --report `"$Out\agent_help_noask.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-help-noask' $agent $agentArgs ''
        $until = (Get-Date).AddSeconds(30)
        while (-not (Test-Path -LiteralPath $idFile) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 300 }
        $helpId = if (Test-Path -LiteralPath $idFile) { (Get-Content -LiteralPath $idFile -Raw).Trim() } else { '' }
        if (-not $helpId) { $failed += 'help-noask agent (no help ID within 30 s)' }
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --seconds 10 --width 640 --mute --help-id $helpId --report `"$Out\viewer_help_noask.txt`""
        Start-SessionTask 'Queqiao-M4-Viewer-help-noask' $viewer $viewerArgs ''
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-help-noask' 90
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-help-noask' 90
        "=== session 'help-noask' (help ID $helpId): agent exit $agentResult, viewer exit $viewerResult"
        $agentText = @(if (Test-Path -LiteralPath "$Out\agent_help_noask.txt") { Get-Content -LiteralPath "$Out\agent_help_noask.txt" -Encoding UTF8 })
        $viewerText = @(if (Test-Path -LiteralPath "$Out\viewer_help_noask.txt") { Get-Content -LiteralPath "$Out\viewer_help_noask.txt" -Encoding UTF8 })
        $agentText | Where-Object { $_ -match '^help' }
        $viewerText | Where-Object { $_ -match 'help|declined|ERROR' }
        if (-not ($agentText | Where-Object { $_ -eq 'help: this PC takes help requests with the help code only' })) { $failed += 'help-noask agent (no code-only line)' }
        if (-not ($agentText | Where-Object { $_ -match 'asked without a code: declined$' })) { $failed += 'help-noask agent (the request was not declined)' }
        if ($agentText | Where-Object { $_ -match '^session 1: ' }) { $failed += 'help-noask agent (a session started)' }
        if (-not ($viewerText | Where-Object { $_ -match 'declined' })) { $failed += 'help-noask viewer (not told it was declined)' }
    }
    # A device taken out of the account during its session (as UU: it is logged out): this PC's agent ends the session
    # at once, though the session itself runs directly between the two programs.
    if ($Sessions -contains 'removed' -or $Sessions -contains 'all') {
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds 120 --report `"$Out\agent_removed.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-removed' $agent $agentArgs ''
        Start-Sleep -Seconds 4
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --test-device m4-removed --device $device --seconds 60 --width 640 --mute --report `"$Out\viewer_removed.txt`""
        Start-SessionTask 'Queqiao-M4-Viewer-removed' $viewer $viewerArgs ''
        Start-Sleep -Seconds 12
        $removedAt = Get-Date
        & $viewer --server $listen --pin $pin --user alice --password $password1 --test-device m4-remover --remove-device m4-removed --report "$Out\remover.txt" | Out-Null
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-removed' 90
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-removed' 90
        "=== session 'removed': agent exit $agentResult, viewer exit $viewerResult"
        $agentText = @(if (Test-Path -LiteralPath "$Out\agent_removed.txt") { Get-Content -LiteralPath "$Out\agent_removed.txt" -Encoding UTF8 })
        $agentText | Where-Object { $_ -match '^(session|ended|RESULT|ERROR)' }
        Get-Content -LiteralPath "$Out\remover.txt" -Encoding UTF8 -ErrorAction SilentlyContinue | Where-Object { $_ -match 'removed|ERROR' }
        if (-not ($agentText | Where-Object { $_ -match '^session 1: viewer m4-removed' })) { $failed += 'removed (no session from the device)' }
        # The device was new to the account: the agent told the person (a notification from its tray icon).
        if (-not ($agentText | Where-Object { $_ -eq 'notification: a new device, m4-removed' })) { $failed += 'removed (no notification of the new device)' }
        if (-not ($agentText | Where-Object { $_ -eq "ended: the viewer's device is no longer on this account, or this PC's login ended" })) { $failed += 'removed (the session did not end on the removal)' }
        # Ended soon after the removal, not when the viewer's 60 s were up.
        if (((Get-Date) - $removedAt).TotalSeconds -gt 40) { $failed += 'removed (the session ended late)' }
    }
    # The lock screen (as UU): a locked PC shows nothing until the viewer gives its Windows password. Simulated here
    # (--test-locked, a test password): nothing is locked, and the person's real Windows account is never tried.
    if ($Sessions -contains 'locked' -or $Sessions -contains 'all') {
        $lockPassword = 'Lock-pass-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds 90 --test-locked --test-password $lockPassword --report `"$Out\agent_locked.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-locked' $agent $agentArgs ''
        Start-Sleep -Seconds 4
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --device $device --seconds 15 --width 640 --mute --test-password wrong-one,$lockPassword --report `"$Out\viewer_locked.txt`""
        Start-SessionTask 'Queqiao-M4-Viewer-locked' $viewer $viewerArgs ''
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-locked' 120
        $agentResult = Wait-SessionTask 'Queqiao-M4-Agent-locked' 60
        "=== session 'locked': agent exit $agentResult, viewer exit $viewerResult"
        $agentText = @(if (Test-Path -LiteralPath "$Out\agent_locked.txt") { Get-Content -LiteralPath "$Out\agent_locked.txt" -Encoding UTF8 })
        $viewerText = @(if (Test-Path -LiteralPath "$Out\viewer_locked.txt") { Get-Content -LiteralPath "$Out\viewer_locked.txt" -Encoding UTF8 })
        $agentText | Where-Object { $_ -match '^(lock|ended|RESULT)' }
        $viewerText | Where-Object { $_ -match '^(password|RESULT)' }
        if (-not ($agentText | Where-Object { $_ -match '^lock: this PC is locked: .*\(5 tries\)$' })) { $failed += 'locked agent (did not ask)' }
        if (-not ($agentText | Where-Object { $_ -eq 'lock: wrong password (4 tries left)' })) { $failed += 'locked agent (the wrong password was not refused)' }
        if (-not ($agentText | Where-Object { $_ -eq "lock: the viewer gave this PC's Windows password (0 frames sent before)" })) { $failed += 'locked agent (pictures went before the password, or it was not taken)' }
        if (-not ($viewerText | Where-Object { $_ -eq 'password: right (4 tries left)' })) { $failed += 'locked viewer (no right answer)' }
        if ($viewerText[-1] -ne 'RESULT PASS') { $failed += 'locked viewer (no pictures after the password)' }
    }
    # Several sessions at once: this PC's agent and a second one (a throwaway device of the same account), and one
    # viewer with a session to each, each on a thread and in a window of its own. No input test: two agents injecting
    # into one PC at once would spoil each other's checks.
    if ($Sessions -contains 'two-at-once' -or $Sessions -contains 'all') {
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --sessions 1 --seconds $($Seconds + 90) --report `"$Out\agent_two_1.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-two-1' $agent $agentArgs ''
        $agentArgs = "--server $listen --pin $pin --user alice --password $password1 --test-device m4-second --sessions 1 --seconds $($Seconds + 90) --report `"$Out\agent_two_2.txt`""
        Start-SessionTask 'Queqiao-M4-Agent-two-2' $agent $agentArgs ''
        Start-Sleep -Seconds 4
        $viewerArgs = "--server $listen --pin $pin --user alice --password $password1 --device $device --also-device m4-second --seconds $Seconds --width 640 --mute --report `"$Out\viewer_two.txt`""
        Start-SessionTask 'Queqiao-M4-Viewer-two' $viewer $viewerArgs ''
        $viewerResult = Wait-SessionTask 'Queqiao-M4-Viewer-two' ($Seconds + 120)
        $agentResults = @(1, 2 | ForEach-Object { Wait-SessionTask "Queqiao-M4-Agent-two-$_" 60 })
        "=== session 'two-at-once': agents exit $($agentResults -join '/'), viewer exit $viewerResult"
        $text = @(Get-Content -LiteralPath "$Out\viewer_two.txt" -Encoding UTF8 -ErrorAction SilentlyContinue)
        $text | Where-Object { $_ -match '^(\d\| (agent|first picture|pictures shown|frames)|sessions at once|RESULT|ERROR)' }
        if ($text[-1] -ne 'RESULT PASS') { $failed += 'two-at-once viewer' }
        if (-not ($text | Where-Object { $_ -match '^sessions at once: 2, all passed: true$' })) { $failed += 'two-at-once viewer (both sessions)' }
        foreach ($at in 1, 2) {
            if (-not ($text | Where-Object { $_ -match "^$at\| agent: " })) { $failed += "two-at-once viewer (no session $at)" }
            $agentText = @(Get-Content -LiteralPath "$Out\agent_two_$at.txt" -ErrorAction SilentlyContinue)
            if ($agentText[-1] -ne 'RESULT PASS') { $failed += "two-at-once agent $at" }
        }
    }
    # A new account from the viewer (this server takes new accounts): logged in at once, its devices listed. A
    # throwaway device: this PC's own belongs to alice by now (one account per device).
    $signupReport = Join-Path $Out 'viewer_signup.txt'
    Remove-Item -LiteralPath $signupReport -ErrorAction SilentlyContinue
    $newUser = 'newbie' + (Get-Random -Maximum 99999)
    & $viewer --server $listen --pin $pin --user $newUser --password 'a-new-password' --signup --test-device m4-newcomer --list --report $signupReport | Out-Null
    $signupText = @(Get-Content -LiteralPath $signupReport -ErrorAction SilentlyContinue)
    '=== a new account from the viewer'
    $signupText
    if ($signupText[-1] -ne 'RESULT PASS') { $failed += 'a new account from the viewer' }
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_err.txt" -ErrorAction SilentlyContinue | Select-Object -Last 20
}
if ($failed) { "M4_LOCAL_FAIL: $($failed -join '; ')" } else { 'M4_LOCAL_PASS' }
