# Sets up a private copy of Soulframe that only talks to the server in sf-server.txt.
# Your real game is only read (copied), never changed or started. Run it with setup.cmd.
param(
    [string]$Source = "$env:LOCALAPPDATA\Soulframe\Downloaded\Public",
    [string]$Dest = "$env:USERPROFILE\SoulframePrivate"
)
$ErrorActionPreference = 'Stop'
$P16H6 = '3be7694b589a7e2ae92a53b3e26ed8c3bcfac8c34ff25d69045df374b35e4765'  # Soulframe.x64.exe, build 2026.10.08.15.20
$official = '\\Soulframe\\Downloaded(\\|$)|\\steamapps\\|\\Epic Games\\'  # same list as bootstrap/src/live.rs

# Resolve paths as the normal user, then elevate once (firewall rules need admin).
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole('Administrators')
if (-not $admin) {
    Start-Process powershell.exe -Verb RunAs -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-NoExit', '-File',
        "`"$PSCommandPath`"", '-Source', "`"$Source`"", '-Dest', "`"$Dest`"")
    exit
}

$here = $PSScriptRoot
# In the sf-join package everything is next to this script; in a clone of the repo (setup.cmd at the top) the
# launcher and DLL come from the cargo build, and with no sf-server.txt the server is this PC.
$bin = if (Test-Path -LiteralPath "$here\sf-launch.exe") { $here } else { "$here\..\..\bootstrap\target\release" }
$isolate = if (Test-Path -LiteralPath "$here\isolate-build.ps1") { "$here\isolate-build.ps1" } else { "$here\..\..\tools\isolate-build.ps1" }
if (-not (Test-Path -LiteralPath "$bin\sf_bootstrap.dll")) { throw "sf_bootstrap.dll not found. From a clone of the repo, run setup.cmd at the top, which builds it." }
$server = if (Test-Path -LiteralPath "$here\sf-server.txt") { (Get-Content -LiteralPath "$here\sf-server.txt" -TotalCount 1).Trim() } else { '127.0.0.1' }
$ip = if ($server -as [ipaddress]) { $server } else {
    ([Net.Dns]::GetHostAddresses($server) | Where-Object AddressFamily -eq 'InterNetwork' | Select-Object -First 1).IPAddressToString
}
if (-not $ip) { throw "Could not find an IPv4 address for '$server' (sf-server.txt)." }
$Dest = [IO.Path]::GetFullPath($Dest)
if ($Dest -match $official) { throw "$Dest is inside a real game install location; pick another -Dest." }

if (-not (Test-Path -LiteralPath "$Dest\Soulframe.x64.exe")) {
    while (-not (Test-Path -LiteralPath "$Source\Soulframe.x64.exe")) {
        $Source = (Read-Host "Soulframe install folder (the one with Soulframe.x64.exe)").Trim('"')
    }
    # Not 'Soulframe*': that also matches SoulframePrivateServer.exe (the GUI that started this setup).
    if (Get-Process -Name 'Soulframe.x64', 'Launcher' -ErrorAction SilentlyContinue | Where-Object { $_.Name -ne 'Launcher' -or $_.Path -match '\\Soulframe\\' }) {
        throw 'Close Soulframe and its launcher first.'
    }
    if ((Get-FileHash -LiteralPath "$Source\Soulframe.x64.exe").Hash.ToLower() -ne $P16H6) {
        throw 'Your Soulframe is a different version than this server supports (P16H6, 2026.10.08). Nothing was changed.'
    }
    "Copying $Source to $Dest (about 13 GB, read-only on your real install)..."
    robocopy $Source $Dest /E /COPY:DAT /DCOPY:T /R:1 /W:1 /NFL /NDL /NP | Out-Host
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }
}
if ((Get-FileHash -LiteralPath "$Dest\Soulframe.x64.exe").Hash.ToLower() -ne $P16H6) { throw "$Dest is not build P16H6." }

# Defender flags sf-launch.exe (it injects sf_bootstrap.dll into the game, as malware loaders do): exclude just these two.
$defender = try { (Get-MpComputerStatus).AntivirusEnabled } catch { $false }
if ($defender) {
    Add-MpPreference -ExclusionPath "$Dest\sf-launch.exe", "$Dest\sf_bootstrap.dll"
}
Copy-Item -LiteralPath "$bin\sf-launch.exe", "$bin\sf_bootstrap.dll" $Dest -Force
Set-Content -LiteralPath "$Dest\sf-server.txt" $ip -Encoding ascii
& $isolate -Path $Dest -Server $ip
""
"Done. Play: double-click play.cmd (repo) or $Dest\sf-launch.exe (never Tools\Launcher.exe, never with your real Soulframe open)."
"Log in with a made-up email and password, NOT your Digital Extremes login."
