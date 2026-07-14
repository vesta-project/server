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

$startBat = Join-Path $target "start-vesta-wrapper.bat"
@"
@echo off
cd /d "%~dp0"
call "%~dp0vesta-wrapper\bin\wrapper.bat" --config "%~dp0wrapper-config.toml"
"@ | Set-Content -Encoding ASCII $startBat

Write-Host ""
Write-Host "Done. Start sync with: $startBat" -ForegroundColor Green
Write-Host "Devtools UI (when running): http://localhost:9090/devtools"
