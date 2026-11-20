# Runs queqiao-server for tests between machines: a fresh data folder, one throw-away user with a random
# password, then the server for -Minutes (it stops when this script ends). The server address, certificate
# fingerprint, user and password are written to <Out>\account.txt for the test programs on the other
# machines. Prints SERVE_READY when the server accepts connections, SERVE_DONE at the end.
# Usage: scripts\serve_test_account.ps1 [-Listen 0.0.0.0:47100] [-Name <IP the others use>] [-Minutes 10]
param(
    [string]$Bin = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\probe\m3_server'),
    [string]$Listen = '0.0.0.0:47100',
    [string]$Name = '',
    [int]$Minutes = 10,
    # Relay also to private and 100.64/10 (Tailscale) addresses.
    [switch]$RelayPrivate
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$data = Join-Path $Out 'server-data'
if (Test-Path -LiteralPath $data) { Remove-Item -Recurse -Force -LiteralPath $data }
$server = Join-Path $Bin 'queqiao-server.exe'
$nameArgs = if ($Name) { @('--name', $Name) } else { @() }
$init = & $server init --data $data @nameArgs
$pin = ([regex]::Match(($init -join "`n"), 'fingerprint: ([0-9a-f]{64})')).Groups[1].Value
if (-not $pin) { "SERVE_FAIL no fingerprint from: $init"; return }
$password = [guid]::NewGuid().ToString('N')
& $server add-user tester $password --data $data
$port = $Listen.Split(':')[-1]
$address = if ($Name) { "${Name}:$port" } else { $Listen }
@("server $address", "pin $pin", 'user tester', "password $password") | Set-Content -LiteralPath (Join-Path $Out 'account.txt') -Encoding ascii
# STUN/TURN on the address the others use (-Name); without it, off.
$turnArgs = if ($Name) { @('--public-ip', $Name) + $(if ($RelayPrivate) { @('--relay-private') } else { @() }) } else { @('--turn-port', '0') }
$process = Start-Process -FilePath $server -ArgumentList (@('serve', '--listen', $Listen, '--data', "`"$data`"") + $turnArgs) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$Out\server_out.txt" -RedirectStandardError "$Out\server_err.txt"
try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $client = New-Object Net.Sockets.TcpClient
        try { $client.Connect('127.0.0.1', [int]$port); $up = $true } catch { $up = $false } finally { $client.Dispose() }
    } while (-not $up -and (Get-Date) -lt $deadline)
    if (-not $up) { 'SERVE_FAIL the server did not start'; return }
    "SERVE_READY $address"
    $stop = (Get-Date).AddMinutes($Minutes)
    while ((Get-Date) -lt $stop -and -not $process.HasExited) { Start-Sleep -Seconds 1 }
} finally {
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    Get-Content -LiteralPath "$Out\server_out.txt", "$Out\server_err.txt" -ErrorAction SilentlyContinue
    'SERVE_DONE'
}
