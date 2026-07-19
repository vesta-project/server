# Package dist/ artifacts locally (wrapper zip + manifest + install scripts).
# Usage: .\scripts\package-dist.ps1 [-OutDir build\dist]
param(
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $Root "build\dist" }

Push-Location $Root
try {
    .\gradlew.bat :wrapper:installDist -q
} finally {
    Pop-Location
}

$installDir = Join-Path $Root "wrapper\build\install\wrapper"
if (-not (Test-Path $installDir)) {
    throw "Missing installDist output at $installDir"
}

if (Test-Path $OutDir) { Remove-Item -Recurse -Force $OutDir }
New-Item -ItemType Directory -Force -Path (Join-Path $OutDir "wrapper") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $OutDir "defaults") | Out-Null

$zipPath = Join-Path $OutDir "wrapper\latest.zip"
if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
Compress-Archive -Path (Join-Path $installDir "*") -DestinationPath $zipPath -Force

$hash = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLowerInvariant()
$gitSha = "unknown"
try { $gitSha = (git -C $Root rev-parse HEAD).Trim() } catch {}
$builtAt = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")

@{
    wrapperSha256 = $hash
    builtAt = $builtAt
    gitSha = $gitSha
} | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $OutDir "manifest.json")

Copy-Item (Join-Path $Root "scripts\install.sh") (Join-Path $OutDir "install.sh")
Copy-Item (Join-Path $Root "scripts\install.ps1") (Join-Path $OutDir "install.ps1")
Copy-Item (Join-Path $Root "scripts\defaults\wrapper-config.toml") (Join-Path $OutDir "defaults\wrapper-config.toml")

Write-Host "Packaged dist artifacts in $OutDir"
Write-Host "  wrapperSha256=$hash"
Write-Host "  gitSha=$gitSha"
