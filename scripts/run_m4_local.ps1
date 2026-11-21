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
# The help session's second account: the probe's lockout checks leave bob locked for 15 minutes.
$password3 = [guid]::NewGuid().ToString('N')
& $server add-user carol $password3 --data $data | Out-Null
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
    $runs = @(
        @('direct', '', "--clipboard-test --dry-run-commands --send-file `"$fileToViewer`"", 'test text from the other side received: yes', 'test text from the other side received: yes', $true),
        @('relay', 'network:relay', '', 'path after .*: relayed through ', 'path after .*: relayed through ', $true),
        @('allowed', '', "--ask --answer allow --privacy-test --ui-pictures `"$Out`"", 'the person at this PC allowed', 'RESULT PASS', $true),
        @('declined', '', '--ask --answer deny --seconds 45', 'the person at this PC declined', 'the person at that PC declined', $false),
        @('ended-here', '', '--end-after 6 --lock-after-session --dry-run-commands', 'ended: ended by the person at this PC', 'ended: the agent ended the session', $true)
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
        if ($label -eq 'relay') { $viewerArgs += ' --terminal-test --game-mouse-test --screen 0' }
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
        $agentArgs = "--server $listen --pin $pin --user carol --password $password3 --test-device m4-helped --sessions 1 --seconds $($Seconds + 90) --verify-input --dry-run-commands --test-press-setting --answer allow --help-code $helpCode --help-id-file `"$idFile`" --report `"$Out\agent_help.txt`""
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
        # What a helper may not do: restart, darken the screen, command lines, sending files.
        $viewerArgs += " --command restart --privacy --terminal-test --send-file `"$fileToAgent`""
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
} finally {
    Stop-Process -Id $serverProcess.Id -Force -ErrorAction SilentlyContinue
    '=== server log'
    Get-Content -LiteralPath "$Out\server_err.txt" -ErrorAction SilentlyContinue | Select-Object -Last 20
}
if ($failed) { "M4_LOCAL_FAIL: $($failed -join '; ')" } else { 'M4_LOCAL_PASS' }
