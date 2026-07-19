# Vesta Server Sync one-liner installer (Windows).
# Production:
#   irm https://raw.githubusercontent.com/vesta-project/server/dist/install.ps1 | iex
# Local / CI:
#   $env:VESTA_DIST_BASE = "file:///C:/path/to/dist"
#   .\scripts\install.ps1 -Root .\vesta-server -SkipPortCheck -NoService
#
# Does NOT install the Vesta connector mod/plugin.

[CmdletBinding()]
param(
    [string]$Root = "",
    [int]$Port = 9090,
    [ValidateSet("", "none", "vanilla", "paper", "fabric", "forge", "neoforge")]
    [string]$Minecraft = "",
    [switch]$SkipPortCheck,
    [switch]$NoService,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"
$DistBase = if ($env:VESTA_DIST_BASE) { $env:VESTA_DIST_BASE.TrimEnd("/") } else {
    "https://raw.githubusercontent.com/vesta-project/server/dist"
}
$McPort = 25565
$RepoHint = "https://github.com/vesta-project/server"

function Write-Info([string]$Message) { Write-Host $Message }
function Write-Warn([string]$Message) { Write-Host "WARNING: $Message" -ForegroundColor Yellow }
function Die([string]$Message) { throw $Message }

function Resolve-FileUri([string]$Url) {
    if ($Url -notlike "file://*") { return $Url }
    $path = $Url.Substring("file://".Length)
    if ($path -match "^/[A-Za-z]:/") { $path = $path.Substring(1) }
    return ($path -replace "/", "\")
}

function Get-Dist([string]$Relative, [string]$OutFile) {
    $url = "$DistBase/$Relative"
    if ($url.StartsWith("file://")) {
        $src = Resolve-FileUri $url
        Copy-Item -Force $src $OutFile
        return
    }
    Invoke-WebRequest -Uri $url -OutFile $OutFile -UseBasicParsing
}

function Test-Java {
    $javaCmd = Get-Command java -ErrorAction SilentlyContinue
    if (-not $javaCmd) {
        Die "Java 17+ is required. Install a JDK, then re-run this installer."
    }
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $lines = & java -version 2>&1 | ForEach-Object { "$_" }
    } finally {
        $ErrorActionPreference = $prev
    }
    Write-Info ("Java: {0}" -f ($lines | Select-Object -First 1))
}

function Get-ServerRoot {
    if ($Root) { return (New-Item -ItemType Directory -Force -Path $Root).FullName }
    $default = Join-Path (Get-Location) "vesta-server"
    if ($Yes) { return (New-Item -ItemType Directory -Force -Path $default).FullName }
    $answer = Read-Host "Server root [$default]"
    if ([string]::IsNullOrWhiteSpace($answer)) { $answer = $default }
    return (New-Item -ItemType Directory -Force -Path $answer).FullName
}

function New-Scaffold([string]$ServerRoot) {
    foreach ($dir in @("mods", "config", "plugins", ".vesta", "vesta-wrapper")) {
        New-Item -ItemType Directory -Force -Path (Join-Path $ServerRoot $dir) | Out-Null
    }
    Write-Info "Scaffolded $ServerRoot"
}

function Install-Wrapper([string]$ServerRoot, [string]$TempDir) {
    $manifestPath = Join-Path $TempDir "manifest.json"
    $zipPath = Join-Path $TempDir "wrapper.zip"
    Get-Dist "manifest.json" $manifestPath
    Get-Dist "wrapper/latest.zip" $zipPath

    $manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
    $expected = [string]$manifest.wrapperSha256
    if (-not $expected) { Die "manifest.json missing wrapperSha256" }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLowerInvariant()
    if ($actual -ne $expected.ToLowerInvariant()) {
        Die "wrapper zip SHA-256 mismatch (expected $expected, got $actual)"
    }

    $wrapperDir = Join-Path $ServerRoot "vesta-wrapper"
    if (Test-Path $wrapperDir) { Remove-Item -Recurse -Force $wrapperDir }
    New-Item -ItemType Directory -Force -Path $wrapperDir | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $wrapperDir -Force

    $bin = Join-Path $wrapperDir "bin\wrapper.bat"
    if (-not (Test-Path $bin)) {
        $nestedBin = Get-ChildItem -Path $wrapperDir -Recurse -Filter "wrapper.bat" | Select-Object -First 1
        if ($nestedBin) {
            $parent = $nestedBin.Directory.Parent.FullName
            Get-ChildItem $parent | ForEach-Object {
                Move-Item -Force $_.FullName (Join-Path $wrapperDir $_.Name)
            }
        }
    }
    if (-not (Test-Path (Join-Path $wrapperDir "bin\wrapper.bat"))) {
        Die "wrapper.zip did not contain bin/wrapper.bat"
    }
    Write-Info "Installed wrapper into $wrapperDir"
}

function Write-Config([string]$ServerRoot, [string]$TempDir) {
    $cfg = Join-Path $ServerRoot "wrapper-config.toml"
    if (Test-Path $cfg) {
        Write-Info "Keeping existing wrapper-config.toml"
        return
    }
    $defaults = Join-Path $TempDir "defaults.toml"
    try {
        Get-Dist "defaults/wrapper-config.toml" $defaults
        Copy-Item $defaults $cfg
    } catch {
        @"
[server]
tcp-port = $Port
http-port = $Port
scan-root = "."
scan-paths = ["mods", "config"]

[server.public]
scheme = "https"
host = "localhost"
port = $Port

[server.tls]
enabled = true
auto-generate = true
keystore = ".vesta/tls.p12"
keystore-password = "vesta-generated"

[server.identity]
name = "Vesta Server"

[modpack]
name = "Server Modpack"
version = "1.0.0"
revision = 1
minimum-revision = 1
minecraft-version = "1.21.4"
source = "self-hosted"
unresolved-policy = "server"

[modpack.modloader]
type = "fabric"
version = "0.18.4"

[modpack.server]
host = "localhost"
port = $McPort

[minecraft]
auto-start = false
launch-jar = "server.jar"
working-directory = "."
java-command = "java"
jvm-args = ["-Xms1G", "-Xmx2G"]
server-args = ["nogui"]

[connector]
auto-update = false

[security]
mode = "open"
password = ""
token-ttl-seconds = 86400
control-port = 9091
anonymous-level = 1
authenticated-level = 1
password-level = 4
linked-level = 1
whitelist-level = 3
"@ | Set-Content -Encoding UTF8 $cfg
    }
    $text = Get-Content $cfg -Raw
    if ($text -notmatch "\[connector\]") {
        Add-Content $cfg "`n[connector]`nauto-update = false`n"
    }
    Write-Info "Wrote $cfg (connector is NOT auto-installed)"
}

function Write-StartScript([string]$ServerRoot) {
    $bat = Join-Path $ServerRoot "start-vesta-wrapper.bat"
    @"
@echo off
cd /d "%~dp0"
call "%~dp0vesta-wrapper\bin\wrapper.bat" --config "%~dp0wrapper-config.toml"
"@ | Set-Content -Encoding ASCII $bat
}

function Resolve-MinecraftChoice {
    if ($Minecraft) { return $Minecraft }
    if ($Yes) { return "none" }
    Write-Host ""
    Write-Host "Optional Minecraft server jar (Vesta connector mod/plugin is NEVER auto-installed):"
    Write-Host "  1) Skip (wrapper-only)"
    Write-Host "  2) Vanilla"
    Write-Host "  3) Paper"
    Write-Host "  4) Fabric"
    Write-Host "  5) Forge (manual)"
    Write-Host "  6) NeoForge (manual)"
    $choice = Read-Host "Choice [1]"
    switch ($choice) {
        "2" { return "vanilla" }
        "3" { return "paper" }
        "4" { return "fabric" }
        "5" { return "forge" }
        "6" { return "neoforge" }
        default { return "none" }
    }
}

function Install-MinecraftJar([string]$ServerRoot, [string]$Choice, [string]$TempDir) {
    $jar = Join-Path $ServerRoot "server.jar"
    switch ($Choice) {
        "none" { Write-Info "Skipping Minecraft jar download"; return }
        "forge" {
            Write-Warn "Automatic Forge install is not automated yet. Place server.jar manually."
            return
        }
        "neoforge" {
            Write-Warn "Automatic NeoForge install is not automated yet. Place server.jar manually."
            return
        }
        "vanilla" {
            $manifest = Join-Path $TempDir "version_manifest.json"
            Invoke-WebRequest -Uri "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json" -OutFile $manifest -UseBasicParsing
            $m = Get-Content $manifest -Raw | ConvertFrom-Json
            $verUrl = ($m.versions | Where-Object { $_.id -eq "1.21.4" } | Select-Object -First 1).url
            $verJson = Join-Path $TempDir "version.json"
            Invoke-WebRequest -Uri $verUrl -OutFile $verJson -UseBasicParsing
            $v = Get-Content $verJson -Raw | ConvertFrom-Json
            Invoke-WebRequest -Uri $v.downloads.server.url -OutFile $jar -UseBasicParsing
            Write-Info "Downloaded vanilla server.jar"
        }
        "paper" {
            $builds = Invoke-RestMethod "https://api.papermc.io/v2/projects/paper/versions/1.21.4/builds"
            $build = $builds.builds[-1].build
            $url = "https://api.papermc.io/v2/projects/paper/versions/1.21.4/builds/$build/downloads/paper-1.21.4-$build.jar"
            Invoke-WebRequest -Uri $url -OutFile $jar -UseBasicParsing
            Write-Info "Downloaded Paper server.jar (build $build)"
        }
        "fabric" {
            $url = "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.9/1.0.1/server/jar"
            Invoke-WebRequest -Uri $url -OutFile $jar -UseBasicParsing
            Write-Info "Downloaded Fabric server.jar"
        }
        default { Die "Unknown Minecraft choice: $Choice" }
    }
}

function Confirm-PortForward {
    if ($SkipPortCheck) {
        Write-Info "Skipping port-forward confirmation (-SkipPortCheck)"
        return
    }
    Write-Host ""
    Write-Host "=== Port forwarding ==="
    Write-Host "Forward TCP $Port on your router/firewall to this machine."
    Write-Host "Local check: https://localhost:$Port/devtools"
    Write-Host "Off-LAN check: https://YOUR_PUBLIC_IP:$Port/devtools (phone on cellular)"
    if ($Yes) {
        Write-Warn "Non-interactive mode: not waiting for port confirmation."
        return
    }
    while ($true) {
        $ans = Read-Host "Have you forwarded TCP $Port and verified access? [y/N/skip]"
        switch -Regex ($ans.ToLowerInvariant()) {
            "^(y|yes)$" { Write-Info "Port forwarding confirmed."; return }
            "^(s|skip)$" { Write-Warn "Continuing without port confirmation."; return }
            default { Write-Info "Forward the port, then answer y - or type skip." }
        }
    }
}

function Test-IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Install-WindowsService([string]$ServerRoot) {
    if ($NoService) {
        Write-Info "Skipping OS service (-NoService)"
        return
    }

    $wrapperBat = Join-Path $ServerRoot "vesta-wrapper\bin\wrapper.bat"
    $config = Join-Path $ServerRoot "wrapper-config.toml"
    $taskName = "VestaWrapper"

    $action = New-ScheduledTaskAction -Execute $wrapperBat -Argument "--config `"$config`"" -WorkingDirectory $ServerRoot
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)

    if (Test-IsAdmin) {
        Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -User "SYSTEM" -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Write-Info ("Registered scheduled task '{0}' as SYSTEM AtStartup" -f $taskName)
    } else {
        $logon = New-ScheduledTaskTrigger -AtLogOn
        Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $logon -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Write-Info ("Registered scheduled task '{0}' for current user AtLogOn" -f $taskName)
        Write-Warn "For machine-wide autostart, re-run this installer from an elevated PowerShell."
    }
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ("vesta-install-" + [guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Force -Path $temp | Out-Null
try {
    Test-Java
    $serverRoot = Get-ServerRoot
    Write-Info "Installing Vesta into: $serverRoot"
    Write-Info "Dist base: $DistBase"
    New-Scaffold $serverRoot
    Install-Wrapper $serverRoot $temp
    Write-Config $serverRoot $temp
    Write-StartScript $serverRoot
    $mc = Resolve-MinecraftChoice
    Install-MinecraftJar $serverRoot $mc $temp
    Confirm-PortForward
    Install-WindowsService $serverRoot

    Write-Host ""
    Write-Host "Done."
    Write-Host "  Start manually:  $(Join-Path $serverRoot 'start-vesta-wrapper.bat')"
    Write-Host "  Devtools:        https://localhost:$Port/devtools"
    Write-Host "  Docs:            $RepoHint"
    Write-Host ""
    Write-Host "The Vesta connector mod/plugin was NOT installed."
    Write-Host "Set connector.auto-update = true only when you intentionally opt in later."
}
finally {
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
