# Install Vesta sync wrapper into an existing Minecraft server directory.
# Usage: .\install-wrapper.ps1 -ServerRoot "C:\path\to\server" [-BackupAck]

param(
    [Parameter(Mandatory = $true)]
    [string]$ServerRoot,
    [switch]$BackupAck
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $repoRoot "wrapper\build\install\wrapper"

if (-not $BackupAck) {
    Write-Host ""
    Write-Host "WARNING: Back up mods/, config/, and worlds before continuing." -ForegroundColor Yellow
    Write-Host "Re-run with -BackupAck once you have a backup." -ForegroundColor Yellow
    exit 1
}

if (-not (Test-Path $dist)) {
    Write-Host "Building wrapper..." -ForegroundColor Cyan
    Push-Location $repoRoot
    .\gradlew.bat :wrapper:installDist
    Pop-Location
}

$target = Resolve-Path $ServerRoot
Write-Host "Installing wrapper into $target"

$wrapperDir = Join-Path $target "vesta-wrapper"
New-Item -ItemType Directory -Force -Path $wrapperDir | Out-Null
Copy-Item -Recurse -Force (Join-Path $dist "*") $wrapperDir

$config = Join-Path $target "wrapper-config.toml"
if (-not (Test-Path $config)) {
    Copy-Item (Join-Path $repoRoot "wrapper\src\main\resources\wrapper-config.toml") $config
    Write-Host "Created wrapper-config.toml — edit security.mode before exposing to LAN."
}

# Ensure a stable identity UUID exists for registry/instance identity.
function Ensure-ServerIdentityId([string]$ConfigPath) {
    $raw = Get-Content $ConfigPath -Raw
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
            if ($section -eq "server.identity") { $identityHeaderIndex = $i }
            continue
        }
        if ($section -eq "server.identity" -and $line -match "^\s*id\s*=") {
            $identityIdFound = $true
        }
    }

    if (-not $identityIdFound) {
        if ($identityHeaderIndex -ge 0) {
            $insertLine = "id = ""$newId"""
            $lines = @($lines[0..$identityHeaderIndex]) + @($insertLine) + @($lines[($identityHeaderIndex+1)..($lines.Count-1)])
        } else {
            $lines += ""
            $lines += "[server.identity]"
            $lines += "name = ""Vesta Server"""
            $lines += "id = ""$newId"""
        }
        Set-Content -Path $ConfigPath -Value ($lines -join "`r`n") -Encoding UTF8
    }
}

# Read configured HTTP port for nicer devtools output.
function Get-HttpPort([string]$ConfigPath) {
    $raw = Get-Content $ConfigPath -Raw
    if ($raw -match "(?m)^\s*http-port\s*=\s*(\d+)\s*$") { return [int]$matches[1] }
    return 9090
}

Ensure-ServerIdentityId $config
$httpPort = Get-HttpPort $config

$startBat = Join-Path $target "start-vesta-wrapper.bat"
@"
@echo off
cd /d "%~dp0"
call "%~dp0vesta-wrapper\bin\wrapper.bat" --config "%~dp0wrapper-config.toml"
"@ | Set-Content -Encoding ASCII $startBat

Write-Host ""
Write-Host "Done. Start sync with: $startBat" -ForegroundColor Green
Write-Host "Devtools UI (when running): https://localhost:$httpPort/devtools"
