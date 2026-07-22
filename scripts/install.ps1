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
    [Alias("instance")] [string]$Instance = "",
    [ValidateSet("", "none", "vanilla", "paper", "fabric", "forge", "neoforge")]
    [string]$Minecraft = "",
    [Alias("existing")] [switch]$Existing,
    [switch]$SkipPortCheck,
    [switch]$NoService,
    [Alias("backup")] [switch]$Backup,
    [Alias("i-understand-risk")] [switch]$IUnderstandRisk,
    [Alias("uninstall")] [switch]$Uninstall,
    [Alias("minecraft-auto-start")] [switch]$MinecraftAutoStart,
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

function Slugify-Instance([string]$Value) {
    $s = $Value.ToLowerInvariant()
    $s = $s -replace "[^a-z0-9]+", "-"
    $s = $s -replace "^-+", ""
    $s = $s -replace "-+$", ""
    $s = $s -replace "-{2,}", "-"
    return $s
}

function Get-InstanceSlug([string]$ServerRoot) {
    if ($Instance -and -not [string]::IsNullOrWhiteSpace($Instance)) { return (Slugify-Instance $Instance) }
    return (Slugify-Instance ([IO.Path]::GetFileName($ServerRoot)))
}

function Get-RegistryPath {
    $dir = Join-Path $env:USERPROFILE ".vesta"
    $path = Join-Path $dir "instances.json"
    return $path
}

function Test-PortBusy([int]$PortToCheck) {
    try {
        $result = Test-NetConnection -ComputerName "127.0.0.1" -Port $PortToCheck -InformationLevel Quiet -WarningAction SilentlyContinue
        return [bool]$result
    } catch {
        $client = New-Object System.Net.Sockets.TcpClient
        $iar = $client.BeginConnect("127.0.0.1", $PortToCheck, $null, $null)
        $success = $iar.AsyncWaitHandle.WaitOne(200, $false)
        if ($success) { $null = $client.EndConnect($iar) }
        $client.Close()
        return [bool]$success
    }
}

function Choose-FreePortPair([int]$HttpPort, [int]$ControlPort) {
    $tries = 0
    while ($true) {
        $httpBusy = Test-PortBusy $HttpPort
        $ctlBusy = Test-PortBusy $ControlPort
        if (-not $httpBusy -and -not $ctlBusy) {
            return [pscustomobject]@{ HttpPort = $HttpPort; ControlPort = $ControlPort }
        }

        $tries++
        if ($tries -gt 100) {
            Die "Could not find free HTTP/control ports after many attempts (last: $HttpPort/$ControlPort)"
        }

        if ($Yes) {
            Write-Warn "Ports busy on localhost ($HttpPort/$ControlPort). Trying next pair..."
            $HttpPort = $HttpPort + 1
            $ControlPort = $ControlPort + 1
            continue
        }

        $nextHttp = $HttpPort + 1
        $nextCtl = $ControlPort + 1
        $ans = Read-Host "Port conflict on localhost for HTTP=$HttpPort / control=$ControlPort. Try next pair ($nextHttp/$nextCtl)? [Y/n]"
        if ([string]::IsNullOrWhiteSpace($ans) -or $ans.ToLowerInvariant().StartsWith("y")) {
            $HttpPort = $nextHttp
            $ControlPort = $nextCtl
        } else {
            Die "Aborting due to port conflict."
        }
    }
}

function Detect-MinecraftLaunchJar([string]$ServerRoot) {
    $serverJar = Join-Path $ServerRoot "server.jar"
    if (Test-Path $serverJar) { return "server.jar" }

    $candidates = @()
    Get-ChildItem -Path $ServerRoot -Filter "*.jar" -File -ErrorAction SilentlyContinue | ForEach-Object {
        $name = $_.Name
        if ($name -like "*sources*.jar" -or $name -like "*javadoc*.jar") { return }
        if ($name -match "paper|fabric|forge|neoforge|server") { $candidates += $_ }
    }

    $newest = $candidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($newest) { return $newest.Name }

    # Fallback: wrapper can run only if the jar exists, but minecraft.auto-start stays false unless opted in.
    return "server.jar"
}

function Extract-WrapperConfigFields([string]$ConfigPath) {
    $out = [pscustomobject]@{
        HttpPort = 0
        ControlPort = 0
        HasIdentityId = $false
        IdentityId = ""
        ConnectorAutoUpdate = $false
        MinecraftAutoStart = $false
        MinecraftLaunchJar = "server.jar"
    }
    if (-not (Test-Path $ConfigPath)) { return $out }

    $lines = Get-Content $ConfigPath -Raw -ErrorAction SilentlyContinue
    $section = ""
    foreach ($line in ($lines -split "`r?`n")) {
        if ($line -match "^\s*#") { continue }
        if ($line -match "^\s*\[(.+)\]\s*$") {
            $section = $matches[1]
            continue
        }

        switch ($section) {
            "server" {
                if ($line -match "^\s*http-port\s*=\s*(\d+)\s*$") { $out.HttpPort = [int]$matches[1] }
            }
            "security" {
                if ($line -match "^\s*control-port\s*=\s*(\d+)\s*$") { $out.ControlPort = [int]$matches[1] }
            }
            "connector" {
                if ($line -match "^\s*auto-update\s*=\s*(true|false)\s*$") {
                    $out.ConnectorAutoUpdate = ($matches[1] -eq "true")
                }
            }
            "server.identity" {
                if ($line -match "^\s*id\s*=\s*""?([^""]+)""?\s*$") { 
                    $out.HasIdentityId = $true
                    $out.IdentityId = $matches[1]
                }
            }
            "minecraft" {
                if ($line -match "^\s*auto-start\s*=\s*(true|false)\s*$") {
                    $out.MinecraftAutoStart = ($matches[1] -eq "true")
                }
                if ($line -match "^\s*launch-jar\s*=\s*""?([^""]+)""?\s*$") {
                    $out.MinecraftLaunchJar = $matches[1]
                }
            }
        }
    }
    return $out
}

function Invoke-StatusProbe([int]$HttpPort) {
    $callback = [System.Net.ServicePointManager]::ServerCertificateValidationCallback
    [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { param($sender, $certificate, $chain, $sslPolicyErrors) return $true }
    try {
        $uri = "https://localhost:$HttpPort/api/v1/status"
        $resp = Invoke-RestMethod -Method GET -Uri $uri -UseBasicParsing -TimeoutSec 2 -ErrorAction Stop
        return $true
    } catch {
        return $false
    } finally {
        [System.Net.ServicePointManager]::ServerCertificateValidationCallback = $callback
    }
}

function Wait-For-Status([int]$HttpPort, [int]$Attempts=30) {
    for ($i=0; $i -lt $Attempts; $i++) {
        if (Invoke-StatusProbe $HttpPort) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

function Create-Backup([string]$ServerRoot) {
    $ts = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
    $out = Join-Path $ServerRoot ("vesta-backup-$ts.zip")

    $paths = @()
    $p1 = Join-Path $ServerRoot "mods"
    if (Test-Path $p1) { $paths += $p1 }
    $p2 = Join-Path $ServerRoot "config"
    if (Test-Path $p2) { $paths += $p2 }
    $p3 = Join-Path $ServerRoot "wrapper-config.toml"
    if (Test-Path $p3) { $paths += $p3 }
    if ($paths.Count -eq 0) { Die "No mods/config/wrapper-config found to back up." }

    if (Get-Command Compress-Archive -ErrorAction SilentlyContinue) {
        if (Test-Path $out) { Remove-Item $out -Force }
        Compress-Archive -Path $paths -DestinationPath $out -Force
        Write-Info "Backup created: $out"
    } else {
        Die "Compress-Archive not available on this PowerShell; cannot create --backup zip."
    }
}

function Registry-Upsert(
    [string]$InstanceName,
    [string]$ConfigPath,
    [string]$RootPath,
    [string]$ServerId,
    [string]$ServiceName
) {
    $regPath = Get-RegistryPath
    $regDir = Split-Path $regPath -Parent
    if (-not (Test-Path $regDir)) { New-Item -ItemType Directory -Force -Path $regDir | Out-Null }

    $data = $null
    if (Test-Path $regPath) {
        $data = Get-Content $regPath -Raw | ConvertFrom-Json
    } else {
        $data = [pscustomobject]@{ version = 1; instances = @() }
    }
    if ($null -eq $data.instances) { $data.instances = @() }

    $canonicalConfig = (Resolve-Path $ConfigPath).Path
    $now = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

    $existing = $null
    foreach ($inst in @($data.instances)) {
        if ($inst.config -eq $canonicalConfig) { $existing = $inst; break }
    }

    if ($existing -ne $null) {
        $existing.name = $InstanceName
        $existing.root = $RootPath
        if (-not [string]::IsNullOrWhiteSpace($ServerId)) { $existing.server_id = $ServerId }
        $existing.service_name = $ServiceName
        # Ports are identity/discovery only; remove any legacy port fields.
        $existing.PSObject.Properties.Remove("http_port") | Out-Null
        $existing.PSObject.Properties.Remove("control_port") | Out-Null
        # Keep existing registered_at.
    } else {
        $newInst = [pscustomobject]@{
            name = $InstanceName
            config = $canonicalConfig
            root = $RootPath
            server_id = $ServerId
            service_name = $ServiceName
            registered_at = $now
        }
        if ([string]::IsNullOrWhiteSpace($ServerId)) { $newInst.PSObject.Properties.Remove("server_id") | Out-Null }
        # Append
        $data.instances = @($data.instances) + @($newInst)
    }

    $tmp = "$regPath.tmp"
    $json = $data | ConvertTo-Json -Depth 20
    Set-Content -Path $tmp -Value $json -Encoding UTF8
    Move-Item -Path $tmp -Destination $regPath -Force
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

    if (-not (Test-Path $cfg)) {
        $defaults = Join-Path $TempDir "defaults.toml"
        Get-Dist "defaults/wrapper-config.toml" $defaults
        Copy-Item $defaults $cfg
    } else {
        Write-Info "Patching existing wrapper-config.toml"
    }

    $desiredHttpPort = $Port
    $desiredControlPort = $ControlPort
    $desiredAutoStart = [bool]$MinecraftAutoStart
    $desiredLaunchJar = if ($MinecraftLaunchJar) { $MinecraftLaunchJar } else { "server.jar" }

    $raw = Get-Content $cfg -Raw
    $lines = $raw -split "`r?`n"
    $section = ""
    $identityHeaderIndex = -1
    $identityIdFound = $false
    $newId = ([guid]::NewGuid().ToString())

    for ($i=0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match "^\s*#") { continue }

        if ($line -match "^\s*\[(.+)\]\s*$") {
            $section = $matches[1].Trim()
            if ($section -eq "server.identity") {
                $identityHeaderIndex = $i
            }
            continue
        }

        switch ($section) {
            "server" {
                if ($line -match "^\s*tcp-port\s*=\s*\d+\s*$") {
                    $lines[$i] = ("tcp-port = {0}" -f $desiredHttpPort)
                }
                if ($line -match "^\s*http-port\s*=\s*\d+\s*$") {
                    $lines[$i] = ("http-port = {0}" -f $desiredHttpPort)
                }
            }
            "server.public" {
                if ($line -match "^\s*port\s*=\s*\d+\s*$") {
                    $lines[$i] = ("port = {0}" -f $desiredHttpPort)
                }
            }
            "security" {
                if ($line -match "^\s*control-port\s*=\s*\d+\s*$") {
                    $lines[$i] = ("control-port = {0}" -f $desiredControlPort)
                }
            }
            "connector" {
                if ($line -match "^\s*auto-update\s*=\s*(true|false)\s*$") {
                    $lines[$i] = "auto-update = false"
                }
            }
            "minecraft" {
                if ($line -match "^\s*auto-start\s*=\s*(true|false)\s*$") {
                    $lines[$i] = ("auto-start = {0}" -f ($desiredAutoStart.ToString().ToLowerInvariant()))
                }
                if ($line -match "^\s*launch-jar\s*=\s*.+$") {
                    $lines[$i] = ("launch-jar = ""{0}""" -f $desiredLaunchJar)
                }
            }
            "server.identity" {
                if ($line -match "^\s*id\s*=\s*.+$") {
                    if ($line -notmatch "^\s*#") { $identityIdFound = $true }
                }
            }
        }
    }

    # Insert id under [server.identity] if missing.
    if (-not $identityIdFound) {
        if ($identityHeaderIndex -ge 0) {
            $insertLine = ("id = ""{0}""" -f $newId)
            $lines = @($lines[0..$identityHeaderIndex]) + @($insertLine) + @($lines[($identityHeaderIndex+1)..($lines.Count-1)])
        } else {
            $lines += ""
            $lines += "[server.identity]"
            $lines += "name = ""Vesta Server"""
            $lines += ("id = ""{0}""" -f $newId)
        }
    }

    # Ensure connector.auto-update=false exists even if [connector] header was missing.
    $text2 = ($lines -join "`r`n")
    if ($text2 -notmatch "\[connector\]") {
        $text2 += "`r`n[connector]`nauto-update = false`r`n"
    }

    # Ensure minecraft section has a launch-jar; if not present, leave it unchanged (auto-start default stays false).
    Set-Content -Path $cfg -Value $text2 -Encoding UTF8
    Write-Info "Wrote/Patched $cfg (ports + identity.id; connector auto-update=false)"
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
    $taskName = "vesta-wrapper-$InstanceSlug"

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
    $InstanceSlug = Get-InstanceSlug $serverRoot
    $serviceName = "vesta-wrapper-$InstanceSlug"

    Write-Info "Installing Vesta into: $serverRoot"
    Write-Info "Instance: $InstanceSlug"
    Write-Info "Dist base: $DistBase"

    if ($Uninstall) {
        $cmd = Get-Command vesta-wrapperctl -ErrorAction SilentlyContinue
        if (-not $cmd) { Die "vesta-wrapperctl not found. Install/enable it (Wave 2) to use --uninstall." }
        Write-Info "Delegating uninstall to vesta-wrapperctl (instance: $InstanceSlug)..."
        & vesta-wrapperctl uninstall --instance $InstanceSlug
        return
    }

    # Existing-server mode: detect server.jar/worlds unless operator explicitly set a mode.
    $existingMode = [bool]$Existing
    if (-not $existingMode) {
        $hasJar = Test-Path (Join-Path $serverRoot "server.jar")
        $hasWorlds = (Test-Path (Join-Path $serverRoot "worlds")) -or (Test-Path (Join-Path $serverRoot "world"))
        if ($hasJar -or $hasWorlds) {
            if ($Yes) {
                $existingMode = $true
            } else {
                $ans = Read-Host "Existing Minecraft server detected. Use --existing mode? [y/N]"
                if ($ans -and ($ans.ToLowerInvariant().StartsWith("y"))) { $existingMode = $true }
            }
        }
    }

    # Desired minecraft settings (auto-start stays false unless opted-in).
    $MinecraftLaunchJar = "server.jar"
    if ($existingMode) {
        $MinecraftLaunchJar = Detect-MinecraftLaunchJar $serverRoot
        if (-not $MinecraftAutoStart -and -not $Yes) {
            $ans = Read-Host "Enable minecraft.auto-start in wrapper-config for this existing server? [y/N]"
            if ($ans -and ($ans.ToLowerInvariant().StartsWith("y"))) { $MinecraftAutoStart = $true }
        }
        Write-Info "Existing server mode: launch-jar='$MinecraftLaunchJar', minecraft.auto-start=$MinecraftAutoStart"
        Write-Info "Suggestion: consider scan-root separation via a 'vesta-pack/' folder."
    } else {
        $MinecraftAutoStart = $false
        $MinecraftLaunchJar = "server.jar"
    }

    # Extract initial ports from existing wrapper-config (if present).
    $cfgPath = Join-Path $serverRoot "wrapper-config.toml"
    $haveCfg = Test-Path $cfgPath
    $cfgFields = $null
    if ($haveCfg) { $cfgFields = Extract-WrapperConfigFields $cfgPath }

    if ($cfgFields -and $cfgFields.HttpPort -gt 0) { $Port = $cfgFields.HttpPort }
    $ControlPort = if ($cfgFields -and $cfgFields.ControlPort -gt 0) { $cfgFields.ControlPort } else { $Port + 1 }

    # Port preflight: probe HTTP/control ports and choose alternate pair if busy.
    $ports = Choose-FreePortPair -HttpPort $Port -ControlPort $ControlPort
    $Port = [int]$ports.HttpPort
    $ControlPort = [int]$ports.ControlPort

    # Backup/risk gating (installer will mutate existing wrapper files/config).
    $needsMutation = $false
    if (Test-Path (Join-Path $serverRoot "vesta-wrapper")) { $needsMutation = $true }

    if ($haveCfg) {
        $configNeedsMutation = $false
        if ($cfgFields.HttpPort -ne $Port -or $cfgFields.ControlPort -ne $ControlPort) { $configNeedsMutation = $true }
        if (-not $cfgFields.HasIdentityId) { $configNeedsMutation = $true }
        if ($cfgFields.ConnectorAutoUpdate -ne $false) { $configNeedsMutation = $true }
        if ($cfgFields.MinecraftAutoStart -ne [bool]$MinecraftAutoStart) { $configNeedsMutation = $true }
        if ($cfgFields.MinecraftLaunchJar -ne $MinecraftLaunchJar) { $configNeedsMutation = $true }
        if ($configNeedsMutation) { $needsMutation = $true }
    }

    if ($needsMutation -and -not $Backup -and -not $IUnderstandRisk) {
        Die "Installer will mutate existing wrapper files/config. Re-run with --backup or --i-understand-risk."
    }
    if ($needsMutation -and $Backup) {
        Create-Backup $serverRoot
    }

    # Install wrapper + patch wrapper-config for this instance's selected ports.
    New-Scaffold $serverRoot
    Install-Wrapper $serverRoot $temp
    Write-Config $serverRoot $temp
    Write-StartScript $serverRoot

    if (-not $existingMode) {
        $mc = Resolve-MinecraftChoice
        Install-MinecraftJar $serverRoot $mc $temp
    } else {
        Write-Info "Existing server mode: not downloading a new minecraft jar."
    }

    Confirm-PortForward
    Install-WindowsService $serverRoot

    # Post-install verify: hit GET /api/v1/status.
    $ok = Wait-For-Status $Port
    if (-not $ok -and $NoService) {
        Write-Warn "No-service mode: starting wrapper briefly for verification..."
        $wrapperBat = Join-Path $serverRoot "vesta-wrapper\bin\wrapper.bat"
        $configFile = Join-Path $serverRoot "wrapper-config.toml"
        if (Test-Path $wrapperBat) {
            $proc = Start-Process -FilePath $wrapperBat -ArgumentList "--config `"$configFile`"" -WorkingDirectory $serverRoot -WindowStyle Hidden -PassThru
            $ok = Wait-For-Status $Port 10
            try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
    }

    if ($ok) {
        Write-Info "Verified: wrapper responded on /api/v1/status (instance: $InstanceSlug)."
    } else {
        Write-Warn "Verify step failed: wrapper did not respond on /api/v1/status yet."
    }

    # Upsert instance entry (identity/discovery only; ports resolved from wrapper-config at runtime).
    $finalFields = Extract-WrapperConfigFields $cfgPath
    $serverId = if ($finalFields -and $finalFields.IdentityId) { $finalFields.IdentityId } else { "" }
    Registry-Upsert $InstanceSlug $cfgPath $serverRoot $serverId $serviceName

    Write-Host ""
    Write-Host "Done."
    Write-Host "  Instance:       $InstanceSlug"
    Write-Host "  Start manually: $(Join-Path $serverRoot 'start-vesta-wrapper.bat')"
    Write-Host "  Devtools:       https://localhost:$Port/devtools"
    Write-Host "  Status:         https://localhost:$Port/api/v1/status"
    Write-Host "  Next steps:     vesta-wrapperctl status --instance $InstanceSlug"
    Write-Host "  Docs:           $RepoHint"
    Write-Host ""
    Write-Host "The Vesta connector mod/plugin was NOT installed."
    Write-Host "Set connector.auto-update = true only when you intentionally opt in later."
}
finally {
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
