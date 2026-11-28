# Builds Queqiao's virtual display driver (drivers\queqiao-idd: Super Screen) and its test tool with the Visual Studio
# Build Tools and the Windows Driver Kit (cl/link directly, no WDK MSBuild integration needed), then signs them with
# Queqiao's code-signing certificate (CurrentUser\My, as scripts\sign.ps1):
#   <Out>\build\     QueqiaoIdd.dll + .pdb, qqidd.exe + .pdb, objects
#   <Out>\package\   what the installer carries: QueqiaoIdd.inf, QueqiaoIdd.dll, QueqiaoIdd.cat (signed) and
#                     queqiao-signing.cer (the certificate's PUBLIC part, which the installer trusts on the machine)
#   <Out>\qqidd.exe   the test tool (signed; not shipped)
# scripts\package.ps1 runs this for the installer. Default <Out>: target\queqiao-idd of the repository.
# Usage: drivers\queqiao-idd\scripts\build.ps1 [-Out <folder>] [-NoSign] [-NoTimestamp] [-Subject 'CN=Queqiao Test Signing']
# Prints DRIVER_VERSION <x.y.z.w>, then BUILD_PASS or BUILD_FAIL <reason>. ASCII only.
param([string]$Out = '', [switch]$NoSign, [switch]$NoTimestamp, [string]$Subject = 'CN=Queqiao Test Signing',
      [string]$TimestampUrl = 'http://timestamp.digicert.com')
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$repo = Split-Path -Parent (Split-Path -Parent $root)
$out = if ($Out) { $Out } else { Join-Path $repo 'target\queqiao-idd' }
$started = Get-Date
function Step($text) { Write-Host ('[{0,6:0.0}s] {1}' -f ((Get-Date) - $started).TotalSeconds, $text) }

# ---- toolchain ----
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { 'BUILD_FAIL no Visual Studio C++ tools'; return }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($Matches[1])" -Value $Matches[2] }
}
$kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$sdk = (Get-ChildItem (Join-Path $kits 'Include') -Directory | Where-Object Name -match '^10\.' |
        Where-Object { Test-Path (Join-Path $_.FullName 'um\iddcx\1.2\IddCx.h') } | Sort-Object Name -Descending |
        Select-Object -First 1).Name
if (-not $sdk) { 'BUILD_FAIL no Windows Driver Kit with IddCx 1.2 headers'; return }
$umdf = '2.25'
$wdfInc = Join-Path $kits "Include\wdf\umdf\$umdf"
$wdfLib = Join-Path $kits "Lib\wdf\umdf\x64\$umdf\WdfDriverStubUm.lib"
$iddInc = Join-Path $kits "Include\$sdk\um\iddcx\1.2"
$iddLib = Join-Path $kits "Lib\$sdk\um\x64\iddcx\1.2\IddCxStub.lib"
$binDir = Join-Path $kits "bin\$sdk"
$signtool = Join-Path $binDir 'x64\signtool.exe'
$inf2cat = Join-Path $binDir 'x86\Inf2Cat.exe'
$infverif = Join-Path $kits "Tools\$sdk\x64\infverif.exe"
foreach ($f in $wdfInc, $wdfLib, $iddInc, $iddLib, $signtool, $inf2cat) { if (-not (Test-Path $f)) { "BUILD_FAIL missing $f"; return } }
Step "toolchain: $vs, kit $sdk, UMDF $umdf, IddCx 1.2, cl $((Get-Command cl.exe).Source)"

$build = Join-Path $out 'build'
$objDriver = Join-Path $build 'obj_driver'
$objTool = Join-Path $build 'obj_tool'
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $objDriver, $objTool | Out-Null

function Run($exe, [string[]]$arguments) {
    $output = & $exe @arguments 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $output | Where-Object { $_ -and $_ -notmatch '^(edid|frames|driver|qqidd)\.cpp$' } | ForEach-Object { Write-Host "    $_" }
    return $code
}

# ---- driver ----
$common = @('/nologo', '/c', '/W4', '/WX', '/O2', '/MT', '/EHsc', '/GS', '/guard:cf', '/Zi', '/std:c++17', '/permissive-',
    '/Zc:wchar_t', '/DUNICODE', '/D_UNICODE', '/D_WIN64', '/D_AMD64_', '/DAMD64', '/D_WIN32_WINNT=0x0A00', '/DWINVER=0x0A00',
    '/DNTDDI_VERSION=0x0A000005')
$driverArgs = $common + @('/DUMDF_DRIVER', '/DUMDF_VERSION_MAJOR=2', '/DUMDF_VERSION_MINOR=25', '/DIDDCX_VERSION_MAJOR=1',
    '/DIDDCX_VERSION_MINOR=2', '/DIDDCX_MINIMUM_VERSION_REQUIRED=2', "/I$wdfInc", "/I$iddInc", "/Fo$objDriver\",
    "/Fd$objDriver\vc.pdb") + @('edid.cpp', 'frames.cpp', 'driver.cpp' | ForEach-Object { Join-Path $root "driver\$_" })
Step 'compiling the driver'
if (Run cl.exe $driverArgs) { 'BUILD_FAIL compiling the driver'; return }
if (Run rc.exe @('/nologo', "/fo$objDriver\QueqiaoIdd.res", (Join-Path $root 'driver\QueqiaoIdd.rc'))) { 'BUILD_FAIL rc'; return }
$dll = Join-Path $build 'QueqiaoIdd.dll'
# /PDBALTPATH: only the PDB's name in the DLL, not the build machine's folders.
$linkArgs = @('/nologo', '/DLL', "/OUT:$dll", "/PDB:$build\QueqiaoIdd.pdb", '/PDBALTPATH:%_PDB%', '/DEBUG', '/OPT:REF', '/OPT:ICF', '/guard:cf',
    '/DYNAMICBASE', '/NXCOMPAT', '/HIGHENTROPYVA', '/SUBSYSTEM:WINDOWS', '/MACHINE:X64') +
    @(Get-ChildItem $objDriver -Filter '*.obj' | ForEach-Object FullName) + @("$objDriver\QueqiaoIdd.res", $wdfLib, $iddLib,
    'OneCoreUAP.lib', 'avrt.lib', 'd3d11.lib', 'dxgi.lib', 'ntdll.lib')
Step 'linking QueqiaoIdd.dll'
if (Run link.exe $linkArgs) { 'BUILD_FAIL linking the driver'; return }
$exports = & dumpbin.exe /nologo /exports $dll | Select-String 'FxDriverEntryUm'
if (-not $exports) { 'BUILD_FAIL QueqiaoIdd.dll does not export FxDriverEntryUm'; return }
Step "exports: $(($exports | ForEach-Object { $_.Line.Trim() }) -join '; ')"
$imports = & dumpbin.exe /nologo /dependents $dll | Where-Object { $_ -match '\.dll\s*$' } | ForEach-Object { $_.Trim() }
Step "imports: $($imports -join ', ')"

# ---- test tool ----
$tool = Join-Path $build 'qqidd.exe'
if (Test-Path (Join-Path $root 'tool\qqidd.cpp')) {
    Step 'compiling qqidd.exe'
    $toolArgs = $common + @("/Fo$objTool\", "/Fd$objTool\vc.pdb", (Join-Path $root 'tool\qqidd.cpp'))
    if (Run cl.exe $toolArgs) { 'BUILD_FAIL compiling the tool'; return }
    $toolLink = @('/nologo', "/OUT:$tool", "/PDB:$build\qqidd.pdb", '/PDBALTPATH:%_PDB%', '/DEBUG', '/OPT:REF', '/guard:cf', '/DYNAMICBASE',
        '/NXCOMPAT', '/SUBSYSTEM:CONSOLE', '/MACHINE:X64', "$objTool\qqidd.obj", 'setupapi.lib', 'cfgmgr32.lib', 'newdev.lib',
        'd3d11.lib', 'dxgi.lib', 'user32.lib', 'gdi32.lib', 'advapi32.lib', 'wtsapi32.lib', 'userenv.lib', 'ole32.lib')
    if (Run link.exe $toolLink) { 'BUILD_FAIL linking the tool'; return }
}

# ---- package ----
$package = Join-Path $out 'package'
New-Item -ItemType Directory -Force $package | Out-Null
Copy-Item (Join-Path $root 'driver\QueqiaoIdd.inf') $package
Copy-Item $dll $package
$driverVer = Select-String -LiteralPath (Join-Path $package 'QueqiaoIdd.inf') -Pattern '^\s*DriverVer\s*=\s*[^,]+,\s*([0-9.]+)' | Select-Object -First 1
if (-not $driverVer) { 'BUILD_FAIL no DriverVer in QueqiaoIdd.inf'; return }
"DRIVER_VERSION $($driverVer.Matches[0].Groups[1].Value)"
if (Test-Path $infverif) {
    Step 'infverif /w (Windows Driver rules)'
    $null = Run $infverif @('/w', (Join-Path $package 'QueqiaoIdd.inf'))
}

if ($NoSign) { Step 'not signed (-NoSign)'; 'BUILD_PASS'; return }

# ---- signing ----
$certificate = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object { $_.Subject -eq $Subject -and $_.NotAfter -gt (Get-Date) -and $_.HasPrivateKey } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $certificate) { "BUILD_FAIL no code-signing certificate '$Subject' with its key in CurrentUser\My"; return }
Step "certificate: $($certificate.Subject), $($certificate.Thumbprint), until $($certificate.NotAfter.ToString('yyyy-MM-dd'))"
function Sign($file) {
    $arguments = @('sign', '/q', '/fd', 'SHA256', '/sha1', $certificate.Thumbprint)
    if (-not $NoTimestamp) { $arguments += @('/tr', $TimestampUrl, '/td', 'SHA256') }
    return (Run $signtool ($arguments + @($file)))
}
Step 'signing QueqiaoIdd.dll and qqidd.exe'
if (Sign (Join-Path $package 'QueqiaoIdd.dll')) { 'BUILD_FAIL signing QueqiaoIdd.dll'; return }
if ((Test-Path $tool) -and (Sign $tool)) { 'BUILD_FAIL signing qqidd.exe'; return }
Step 'Inf2Cat'
$osList = '10_RS4_X64,10_RS5_X64,10_19H1_X64,10_VB_X64,10_CO_X64,10_NI_X64,10_GE_X64,Server10_X64,ServerRS5_X64,ServerFE_X64'
if (Run $inf2cat @("/driver:$package", "/os:$osList", '/uselocaltime')) { 'BUILD_FAIL Inf2Cat'; return }
# Inf2Cat names the catalog in lower case; the INF's CatalogFile is QueqiaoIdd.cat (Windows does not mind, the
# installer's file list does).
$catalog = Join-Path $package 'QueqiaoIdd.cat'
Move-Item -LiteralPath (Join-Path $package 'queqiaoidd.cat') -Destination "$catalog.tmp"
Move-Item -LiteralPath "$catalog.tmp" -Destination $catalog
Step 'signing QueqiaoIdd.cat'
if (Sign $catalog) { 'BUILD_FAIL signing the catalog'; return }
$null = Run $signtool @('verify', '/q', '/pa', '/c', $catalog, (Join-Path $package 'QueqiaoIdd.dll'))
Step "catalog verify (chain trusted on this PC: exit $LASTEXITCODE; 0 only where the certificate is trusted)"
# The public part only (Export-Certificate -Type CERT never holds the private key).
$cer = Join-Path $package 'queqiao-signing.cer'
Export-Certificate -Cert $certificate -FilePath $cer -Type CERT | Out-Null
if ((New-Object Security.Cryptography.X509Certificates.X509Certificate2 $cer).HasPrivateKey) { 'BUILD_FAIL the .cer holds a private key'; return }
if (Test-Path $tool) { Copy-Item $tool $out }
foreach ($f in Get-ChildItem -File $package) {
    $sig = Get-AuthenticodeSignature -LiteralPath $f.FullName
    '{0,-22} {1}  {2}' -f $f.Name, (Get-FileHash -Algorithm SHA256 $f.FullName).Hash.Substring(0, 16),
        $(if ($sig.SignerCertificate) { "signed by $($sig.SignerCertificate.Subject) ($($sig.SignerCertificate.Thumbprint)); timestamp: $([bool]$sig.TimeStamperCertificate)" } else { 'not signed' })
}
'BUILD_PASS'
