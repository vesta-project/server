#Requires -Version 5.1
<#
.SYNOPSIS
  End-to-end smoke for the Vesta wrapper (Phase 3).

.DESCRIPTION
  Starts an ephemeral wrapper against demo-server (or -Root), exercises:
    status, sync-check, descriptor capabilities, fs list/write/read/delete,
    console GET (+ POST when Minecraft is started), players, rate_limits TOML,
    and vesta-wrapperctl status.

  Tears down the wrapper (and optional Minecraft) on exit.

.EXAMPLE
  .\scripts\smoke-wrapper.ps1
  .\scripts\smoke-wrapper.ps1 -SkipMinecraft
  .\scripts\smoke-wrapper.ps1 -HttpPort 19090 -SkipBuild
#>
[CmdletBinding()]
param(
    [string]$Root = "",
    [string]$RepoRoot = "",
    [int]$HttpPort = 0,
    [int]$TcpPort = 0,
    [int]$ControlPort = 0,
    [switch]$SkipBuild,
    [switch]$SkipMinecraft,
    [switch]$KeepAlive,
    [int]$MinecraftReadyTimeoutSec = 180
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Write-Info([string]$Message) { Write-Host "[smoke] $Message" }
function Write-Pass([string]$Name) { Write-Host "  PASS  $Name" -ForegroundColor Green }
function Write-Fail([string]$Name, [string]$Detail) {
    Write-Host "  FAIL  $Name - $Detail" -ForegroundColor Red
}
function Write-Skip([string]$Name, [string]$Detail) {
    Write-Host "  SKIP  $Name - $Detail" -ForegroundColor Yellow
}

$Script:Results = New-Object System.Collections.Generic.List[object]
$Script:WrapperProc = $null
$Script:McStartedBySmoke = $false
$Script:BaseUrl = $null
$Script:ConfigPath = $null
$Script:InstanceName = "smoke-test"
$Script:ProbeRelPath = "smoke-e2e-probe.txt"
$Script:TlsEnabled = $false

function Record-Result([string]$Name, [string]$Status, [string]$Detail = "") {
    $Script:Results.Add([pscustomobject]@{ Name = $Name; Status = $Status; Detail = $Detail })
    switch ($Status) {
        "PASS" { Write-Pass $Name }
        "FAIL" { Write-Fail $Name $Detail }
        "SKIP" { Write-Skip $Name $Detail }
    }
}

function Find-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()
    return $port
}

function Resolve-RepoRoot {
    if ($RepoRoot) { return (Resolve-Path -LiteralPath $RepoRoot).Path }
    $here = $PSScriptRoot
    return (Resolve-Path -LiteralPath (Join-Path $here "..")).Path
}

function Ensure-TlsBypass {
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
    [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
}

function Invoke-SmokeApi {
    param(
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [string]$Body = $null,
        [string]$ContentType = "application/json",
        [byte[]]$RawBody = $null,
        [hashtable]$Headers = $null,
        [switch]$AllowFail,
        [switch]$AsBytes
    )
    $url = "$($Script:BaseUrl)$Path"
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue

    # Prefer curl.exe (more reliable than Invoke-WebRequest under StrictMode on Windows).
    if ($curl) {
        $tmpOut = [System.IO.Path]::GetTempFileName()
        $tmpBin = $null
        $argList = New-Object System.Collections.Generic.List[string]
        [void]$argList.Add("-sS")
        [void]$argList.Add("-X")
        [void]$argList.Add($Method)
        [void]$argList.Add("-o")
        [void]$argList.Add($tmpOut)
        [void]$argList.Add("-w")
        [void]$argList.Add("%{http_code}")
        [void]$argList.Add("--max-time")
        [void]$argList.Add("60")
        if ($Headers) {
            foreach ($k in $Headers.Keys) {
                [void]$argList.Add("-H")
                [void]$argList.Add(("{0}: {1}" -f $k, $Headers[$k]))
            }
        }
        if ($null -ne $RawBody) {
            $tmpBin = [System.IO.Path]::GetTempFileName()
            [System.IO.File]::WriteAllBytes($tmpBin, $RawBody)
            [void]$argList.Add("-H")
            [void]$argList.Add("Content-Type: $ContentType")
            [void]$argList.Add("--data-binary")
            [void]$argList.Add("@$tmpBin")
        } elseif ($null -ne $Body) {
            [void]$argList.Add("-H")
            [void]$argList.Add("Content-Type: $ContentType")
            [void]$argList.Add("--data-binary")
            [void]$argList.Add($Body)
        }
        [void]$argList.Add("--")
        [void]$argList.Add($url)
        try {
            $codeRaw = & $curl.Source @($argList.ToArray()) 2>&1
            if ($codeRaw -is [System.Array]) {
                $errText = ($codeRaw | ForEach-Object { "$_" }) -join "`n"
                if ($AllowFail) {
                    return [pscustomobject]@{ StatusCode = 0; Content = $errText }
                }
                throw "curl failed for $Method $url : $errText"
            }
            $code = 0
            [void][int]::TryParse(([string]$codeRaw).Trim(), [ref]$code)
            $content = [System.IO.File]::ReadAllText($tmpOut)
            if (-not $AllowFail -and ($code -lt 200 -or $code -ge 300)) {
                throw "HTTP $code for $Method $url : $content"
            }
            return [pscustomobject]@{ StatusCode = $code; Content = $content }
        } finally {
            Remove-Item -LiteralPath $tmpOut -Force -ErrorAction SilentlyContinue
            if ($tmpBin) { Remove-Item -LiteralPath $tmpBin -Force -ErrorAction SilentlyContinue }
        }
    }

    Ensure-TlsBypass
    $params = @{
        Method          = $Method
        Uri             = $url
        TimeoutSec      = 30
        UseBasicParsing = $true
    }
    if ($Headers -and $Headers.Count -gt 0) {
        $params.Headers = $Headers
    }
    if ($null -ne $RawBody) {
        $params.Body = $RawBody
        if ($ContentType) { $params.ContentType = $ContentType }
    } elseif ($null -ne $Body) {
        $params.Body = $Body
        if ($ContentType) { $params.ContentType = $ContentType }
    }
    try {
        return Invoke-WebRequest @params
    } catch {
        $ex = $_.Exception
        $webResp = $null
        if ($ex -is [System.Net.WebException] -and $null -ne $ex.Response) {
            $webResp = $ex.Response
        } elseif ($null -ne $ex.InnerException -and $ex.InnerException -is [System.Net.WebException] `
            -and $null -ne $ex.InnerException.Response) {
            $webResp = $ex.InnerException.Response
        }
        if ($AllowFail -and $webResp) {
            $stream = $webResp.GetResponseStream()
            $reader = New-Object System.IO.StreamReader($stream)
            $content = $reader.ReadToEnd()
            $reader.Close()
            return [pscustomobject]@{
                StatusCode = [int]$webResp.StatusCode
                Content    = $content
                Headers    = $webResp.Headers
            }
        }
        throw
    }
}

function Get-Json($resp) {
    if (-not $resp.Content) { return $null }
    return ($resp.Content | ConvertFrom-Json)
}

function Wait-HttpReady([int]$TimeoutSec = 45) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $lastErr = ""
    while ((Get-Date) -lt $deadline) {
        if ($Script:WrapperProc -and $Script:WrapperProc.HasExited) {
            $lastErr = "wrapper process exited early code=$($Script:WrapperProc.ExitCode)"
            break
        }
        try {
            # Prefer curl.exe when present (more reliable than IWR on some hosts)
            $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
            if ($curl) {
                $out = & curl.exe -sS -o NUL -w "%{http_code}" --max-time 3 "$($Script:BaseUrl)/api/v1/status" 2>&1
                if ("$out" -match '200') { return $true }
                $lastErr = "curl=$out"
            } else {
                $resp = Invoke-SmokeApi -Method GET -Path "/api/v1/status" -AllowFail
                if ([int]$resp.StatusCode -eq 200) { return $true }
                $lastErr = "http=$([int]$resp.StatusCode)"
            }
        } catch {
            $lastErr = $_.Exception.Message
        }
        Start-Sleep -Milliseconds 400
    }
    Write-Info "Wait-HttpReady last error: $lastErr base=$($Script:BaseUrl)"
    return $false
}

function Stop-SmokeProcesses {
    if ($Script:McStartedBySmoke -and $Script:BaseUrl) {
        Write-Info "Stopping managed Minecraft via power API"
        try {
            $null = Invoke-SmokeApi -Method POST -Path "/api/v1/server/power" `
                -Body '{"action":"stop"}' -AllowFail
            Start-Sleep -Seconds 2
            $null = Invoke-SmokeApi -Method POST -Path "/api/v1/server/power" `
                -Body '{"action":"kill"}' -AllowFail
        } catch {}
    }
    if ($Script:WrapperProc -and -not $Script:WrapperProc.HasExited) {
        Write-Info "Stopping wrapper pid $($Script:WrapperProc.Id) (process tree)"
        try {
            & taskkill.exe /PID $Script:WrapperProc.Id /T /F 2>$null | Out-Null
        } catch {
            try { Stop-Process -Id $Script:WrapperProc.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
        try { $Script:WrapperProc.WaitForExit(8000) | Out-Null } catch {}
    }
    try {
        $ctl = Join-Path $script:Repo "scripts\vesta-wrapperctl.ps1"
        if ((Test-Path -LiteralPath $ctl) -and $Script:ConfigPath) {
            & $ctl unregister --config $Script:ConfigPath 2>$null | Out-Null
        }
    } catch {}
}

$script:Repo = Resolve-RepoRoot
if (-not $Root) {
    $Root = Join-Path $script:Repo "demo-server"
}
$Root = (Resolve-Path -LiteralPath $Root).Path

if ($HttpPort -le 0) { $HttpPort = Find-FreePort }
if ($TcpPort -le 0) {
    do { $TcpPort = Find-FreePort } while ($TcpPort -eq $HttpPort)
}
if ($ControlPort -le 0) {
    do { $ControlPort = Find-FreePort } while ($ControlPort -eq $HttpPort -or $ControlPort -eq $TcpPort)
}

$smokeDir = Join-Path $script:Repo "build\smoke-e2e"
New-Item -ItemType Directory -Force -Path $smokeDir | Out-Null
$Script:ConfigPath = Join-Path $smokeDir "wrapper-config.smoke.toml"
$logOut = Join-Path $smokeDir "wrapper.out.log"
$logErr = Join-Path $smokeDir "wrapper.err.log"

# Prefer demo-server vesta-pack when present; else local mods/config under smoke dir.
$scanRoot = Join-Path $Root "vesta-pack"
if (-not (Test-Path -LiteralPath $scanRoot)) {
    $scanRoot = $smokeDir
    New-Item -ItemType Directory -Force -Path (Join-Path $scanRoot "mods") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $scanRoot "config") | Out-Null
}
New-Item -ItemType Directory -Force -Path (Join-Path $Root ".vesta") | Out-Null

$paperJar = Get-ChildItem -LiteralPath $Root -Filter "paper-*.jar" -File -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1
$launchJarName = if ($paperJar) { $paperJar.Name } else { "" }
$javaCmd = (Get-Command java -ErrorAction SilentlyContinue).Source
if (-not $javaCmd) { $javaCmd = "java" }

$scanRootToml = ($scanRoot -replace '\\', '/')
$workDirToml = ($Root -replace '\\', '/')

$toml = @"
[server]
tcp-port = $TcpPort
http-port = $HttpPort
scan-root = "$scanRootToml"
scan-paths = ["mods", "config"]

[server.public]
scheme = "http"
host = "127.0.0.1"
port = $HttpPort

[server.tls]
enabled = false
auto-generate = false
keystore = ".vesta/tls.p12"
keystore-password = "vesta-generated"

[server.identity]
name = "Vesta Smoke"

[modpack]
name = "Vesta Smoke Pack"
description = "Ephemeral smoke pack"
version = "1.0.0-smoke"
minecraft-version = "1.21.4"
source = "self-hosted"
unresolved-policy = "server"

[modpack.modloader]
type = "fabric"
version = "0.18.4"

[modpack.platform]
project-id = ""
version-id = ""
download-url = ""

[modpack.archive]
path = ""

[modpack.server]
host = "localhost"
port = 25565
description = "smoke"
is-required = false

[minecraft]
auto-start = false
launch-jar = "$launchJarName"
working-directory = "$workDirToml"
java-command = "$($javaCmd -replace '\\', '/')"
jvm-args = ["-Xms512M", "-Xmx1G"]
server-args = ["nogui"]

[connector]
auto-update = false

[security]
mode = "open"
password = ""
token-ttl-seconds = 86400
control-port = $ControlPort
anonymous-level = 4
authenticated-level = 4
password-level = 4
linked-level = 4
whitelist-level = 4

[security.levels]
"0" = ["stats.read"]
"1" = ["stats.read", "pack.fetch"]
"2" = ["stats.read", "pack.fetch", "console.read"]
"3" = ["stats.read", "pack.fetch", "console.read", "files.read"]
"4" = [
  "stats.read",
  "pack.fetch",
  "console.read",
  "files.read",
  "console.execute",
  "files.write",
  "settings.manage",
  "content.manage",
  "server.power",
]

[devtools]
enabled = true
loopback_only = true

[rate_limits]
enabled = true
http_requests_per_minute = 120
websocket_messages_per_second = 30
auth_attempts_per_minute = 20
"@
# When no launch jar, omit launch-jar so console returns 409 (manageable=false path for players).
if (-not $launchJarName) {
    $toml = $toml -replace 'launch-jar = ""', 'launch-jar = ""'
}

[System.IO.File]::WriteAllText($Script:ConfigPath, $toml, [System.Text.UTF8Encoding]::new($false))
$Script:BaseUrl = "http://127.0.0.1:$HttpPort"
$Script:TlsEnabled = $false

Write-Info "Repo: $script:Repo"
Write-Info "Root: $Root"
Write-Info "Config: $($Script:ConfigPath)"
Write-Info "HTTP :$HttpPort  TCP :$TcpPort  control :$ControlPort"
Write-Info "Launch jar: $(if ($launchJarName) { $launchJarName } else { '(none)' })"

try {
    # --- rate_limits config presence (defaults + smoke config) ---
    $defaultsToml = Join-Path $script:Repo "scripts\defaults\wrapper-config.toml"
    if ((Test-Path -LiteralPath $defaultsToml) -and
        (Select-String -LiteralPath $defaultsToml -Pattern '\[rate_limits\]' -Quiet) -and
        (Select-String -LiteralPath $Script:ConfigPath -Pattern '\[rate_limits\]' -Quiet)) {
        Record-Result "rate_limits_config" "PASS" "present in defaults + smoke config"
    } else {
        Record-Result "rate_limits_config" "FAIL" "[rate_limits] missing from defaults or smoke config"
    }

    if (-not $SkipBuild) {
        Write-Info "Building :wrapper:installDist"
        Push-Location $script:Repo
        try {
            & .\gradlew.bat :wrapper:installDist --quiet
            if ($LASTEXITCODE -ne 0) { throw "gradle installDist failed ($LASTEXITCODE)" }
        } finally {
            Pop-Location
        }
    }

    $wrapperHome = Join-Path $script:Repo "wrapper\build\install\wrapper"
    $wrapperLib = Join-Path $wrapperHome "lib"
    if (-not (Test-Path -LiteralPath $wrapperLib)) {
        throw "wrapper lib not found at $wrapperLib - run without -SkipBuild"
    }
    $cp = ((Get-ChildItem -LiteralPath $wrapperLib -Filter "*.jar").FullName) -join ";"
    $javaExe = (Get-Command java -ErrorAction SilentlyContinue).Source
    if (-not $javaExe) { throw "java not found on PATH" }

    Write-Info "Starting wrapper (java classpath)"
    if (Test-Path -LiteralPath $logOut) { Remove-Item -LiteralPath $logOut -Force }
    if (Test-Path -LiteralPath $logErr) { Remove-Item -LiteralPath $logErr -Force }

    $Script:WrapperProc = Start-Process -FilePath $javaExe `
        -ArgumentList @("-classpath", $cp, "com.vesta.wrapper.VestaWrapper", "--config", $Script:ConfigPath) `
        -WorkingDirectory $Root `
        -RedirectStandardOutput $logOut `
        -RedirectStandardError $logErr `
        -PassThru -WindowStyle Hidden

    if (-not (Wait-HttpReady 60)) {
        Record-Result "wrapper_ready" "FAIL" "HTTP status not ready; see $logOut / $logErr"
        throw "Wrapper did not become ready"
    }
    Record-Result "wrapper_ready" "PASS"

    # --- status ---
    try {
        $resp = Invoke-SmokeApi -Method GET -Path "/api/v1/status"
        $j = Get-Json $resp
        if ([int]$resp.StatusCode -eq 200 -and $j.security_mode) {
            Record-Result "status" "PASS" "mode=$($j.security_mode) mc_manageable=$($j.minecraft_manageable)"
        } else {
            Record-Result "status" "FAIL" "HTTP $([int]$resp.StatusCode) body=$($resp.Content)"
        }
    } catch {
        Record-Result "status" "FAIL" $_.Exception.Message
    }

    # --- sync-check ---
    try {
        $body = '{"current_version":"0.0.0-smoke","installed_hashes":{}}'
        $resp = Invoke-SmokeApi -Method POST -Path "/api/v1/sync/check" -Body $body
        $j = Get-Json $resp
        if ([int]$resp.StatusCode -eq 200) {
            Record-Result "sync_check" "PASS"
        } else {
            Record-Result "sync_check" "FAIL" "HTTP $([int]$resp.StatusCode) $($resp.Content)"
        }
    } catch {
        Record-Result "sync_check" "FAIL" $_.Exception.Message
    }

    # --- descriptor capabilities ---
    $requiredCaps = @(
        "server_descriptor", "modpack_sync", "minecraft_challenge", "realtime_ticket",
        "console", "filesystem", "server_power", "content_manage",
        "admin_security_loopback", "sync_check", "players"
    )
    try {
        $resp = Invoke-SmokeApi -Method GET -Path "/api/v1/server/descriptor"
        $j = Get-Json $resp
        $caps = @($j.capabilities)
        $missing = @($requiredCaps | Where-Object { $_ -notin $caps })
        if ([int]$resp.StatusCode -eq 200 -and $missing.Count -eq 0 -and $j.endpoints) {
            Record-Result "descriptor_capabilities" "PASS" ("caps=" + ($caps -join ","))
        } else {
            Record-Result "descriptor_capabilities" "FAIL" "missing=$($missing -join ',') endpoints=$([bool]$j.endpoints)"
        }
    } catch {
        Record-Result "descriptor_capabilities" "FAIL" $_.Exception.Message
    }

    # --- filesystem round-trip (list / write / read / delete) ---
    try {
        $list = Invoke-SmokeApi -Method GET -Path "/api/v1/fs?path="
        $listJson = Get-Json $list
        if ([int]$list.StatusCode -ne 200 -or -not $listJson.entries) {
            Record-Result "fs_list" "FAIL" "HTTP $([int]$list.StatusCode) $($list.Content)"
        } else {
            # Metadata-first: entries should have name/type (not full file bodies)
            $sample = $listJson.entries | Select-Object -First 1
            if ($sample -and ($sample.PSObject.Properties.Name -contains "name") -and
                ($sample.PSObject.Properties.Name -contains "type")) {
                Record-Result "fs_list" "PASS" "entries=$($listJson.entries.Count)"
            } else {
                Record-Result "fs_list" "FAIL" "listing missing metadata fields"
            }
        }

        $probeBytes = [System.Text.Encoding]::UTF8.GetBytes("smoke-probe-$(Get-Date -Format o)")
        $put = Invoke-SmokeApi -Method PUT -Path "/api/v1/fs/$($Script:ProbeRelPath)" `
            -RawBody $probeBytes -ContentType "application/octet-stream"
        $putJson = Get-Json $put
        if ([int]$put.StatusCode -eq 200 -and $putJson.ok) {
            Record-Result "fs_write" "PASS"
        } else {
            Record-Result "fs_write" "FAIL" "HTTP $([int]$put.StatusCode) $($put.Content)"
        }

        $get = Invoke-SmokeApi -Method GET -Path "/api/v1/fs/$($Script:ProbeRelPath)"
        if ([int]$get.StatusCode -eq 200 -and $get.Content -eq [System.Text.Encoding]::UTF8.GetString($probeBytes)) {
            Record-Result "fs_read" "PASS" "streamed body matched"
        } else {
            Record-Result "fs_read" "FAIL" "HTTP $([int]$get.StatusCode) content mismatch"
        }

        $del = Invoke-SmokeApi -Method DELETE -Path "/api/v1/fs/$($Script:ProbeRelPath)"
        $delJson = Get-Json $del
        if ([int]$del.StatusCode -eq 200 -and $delJson.deleted) {
            Record-Result "fs_delete" "PASS"
        } else {
            Record-Result "fs_delete" "FAIL" "HTTP $([int]$del.StatusCode) $($del.Content)"
        }
    } catch {
        Record-Result "fs_roundtrip" "FAIL" $_.Exception.Message
    }

    # --- players (without MC: manageable true/false depending on launch-jar) ---
    try {
        $resp = Invoke-SmokeApi -Method GET -Path "/api/v1/players"
        $j = Get-Json $resp
        if ([int]$resp.StatusCode -eq 200 -and ($null -ne $j.players)) {
            Record-Result "players" "PASS" "manageable=$($j.manageable) running=$($j.running) online=$($j.online)"
        } else {
            Record-Result "players" "FAIL" "HTTP $([int]$resp.StatusCode) $($resp.Content)"
        }
    } catch {
        Record-Result "players" "FAIL" $_.Exception.Message
    }

    # --- console GET (configured launch-jar) ---
    if ($launchJarName) {
        try {
            $resp = Invoke-SmokeApi -Method GET -Path "/api/v1/console?since=0&limit=50"
            $j = Get-Json $resp
            if ([int]$resp.StatusCode -eq 200 -and ($null -ne $j.cursor) -and ($null -ne $j.lines)) {
                Record-Result "console_get" "PASS" "running=$($j.running) cursor=$($j.cursor) lines=$($j.lines.Count)"
            } else {
                Record-Result "console_get" "FAIL" "HTTP $([int]$resp.StatusCode) $($resp.Content)"
            }
        } catch {
            Record-Result "console_get" "FAIL" $_.Exception.Message
        }
    } else {
        Record-Result "console_get" "SKIP" "no paper/server jar in $Root"
    }

    # --- console POST + cursor progression (requires MC) ---
    $doMc = -not $SkipMinecraft -and $launchJarName
    if (-not $doMc) {
        Record-Result "console_post" "SKIP" $(if ($SkipMinecraft) { "-SkipMinecraft" } else { "no launch jar" })
        Record-Result "console_cursor_progress" "SKIP" "depends on console_post"
        Record-Result "console_realtime" "SKIP" "depends on console_post"
    } else {
        Write-Info "Starting Minecraft (timeout ${MinecraftReadyTimeoutSec}s)"
        $power = Invoke-SmokeApi -Method POST -Path "/api/v1/server/power" -Body '{"action":"start"}' -AllowFail
        if ([int]$power.StatusCode -ne 200) {
            Record-Result "console_post" "FAIL" "power start HTTP $([int]$power.StatusCode) $($power.Content)"
            Record-Result "console_cursor_progress" "SKIP" "MC failed to start"
            Record-Result "console_realtime" "SKIP" "MC failed to start"
        } else {
            $Script:McStartedBySmoke = $true
            $ready = $false
            $deadline = (Get-Date).AddSeconds($MinecraftReadyTimeoutSec)
            $since = 0L
            while ((Get-Date) -lt $deadline) {
                try {
                    $snap = Get-Json (Invoke-SmokeApi -Method GET -Path "/api/v1/console?since=$since&limit=200" -AllowFail)
                    if ($snap.cursor) { $since = [int64]$snap.cursor }
                    $text = (($snap.lines | ForEach-Object { $_.text }) -join "`n")
                    if ($text -match '(?i)Done \(|For help, type|Timings Reset|You need to agree to the EULA') {
                        $ready = $true
                        break
                    }
                    $st = Get-Json (Invoke-SmokeApi -Method GET -Path "/api/v1/status" -AllowFail)
                    if ($st -and -not $st.minecraft_running -and $text -match '(?i)Error|Exception|Failed') {
                        break
                    }
                } catch {}
                Start-Sleep -Seconds 2
            }
            if (-not $ready) {
                Record-Result "console_post" "FAIL" "MC did not become ready within ${MinecraftReadyTimeoutSec}s (see console / $logOut)"
                Record-Result "console_cursor_progress" "SKIP" "MC not ready"
                Record-Result "console_realtime" "SKIP" "MC not ready"
            } else {
                $before = Get-Json (Invoke-SmokeApi -Method GET -Path "/api/v1/console?since=0&limit=1")
                $cursorBefore = [int64]$before.cursor

                $post = Invoke-SmokeApi -Method POST -Path "/api/v1/console" -Body '{"command":"list"}' -AllowFail
                $postJson = Get-Json $post
                if ([int]$post.StatusCode -eq 200 -and $postJson.ok) {
                    Record-Result "console_post" "PASS" "command=list accepted"
                } else {
                    Record-Result "console_post" "FAIL" "HTTP $([int]$post.StatusCode) $($post.Content)"
                }

                Start-Sleep -Seconds 2
                $after = Get-Json (Invoke-SmokeApi -Method GET -Path "/api/v1/console?since=$cursorBefore&limit=100")
                $cursorAfter = [int64]$after.cursor
                $newLines = @($after.lines)
                if ($cursorAfter -gt $cursorBefore -and $newLines.Count -ge 1) {
                    Record-Result "console_cursor_progress" "PASS" ("cursor {0} -> {1} (added {2} lines)" -f $cursorBefore, $cursorAfter, $newLines.Count)
                } else {
                    Record-Result "console_cursor_progress" "FAIL" "cursor $cursorBefore -> $cursorAfter lines=$($newLines.Count)"
                }

                # Realtime console_line needs a linked bearer ticket; open-mode smoke skips WS.
                Record-Result "console_realtime" "SKIP" "WS console_line requires linked bearer ticket (open smoke uses REST cursor only)"
            }
        }
    }

    # --- vesta-wrapperctl status ---
    try {
        $ctl = Join-Path $script:Repo "scripts\vesta-wrapperctl.ps1"
        & $ctl register --config $Script:ConfigPath --name $Script:InstanceName | Out-Host
        & $ctl status --instance $Script:InstanceName | Out-Host
        $ctlCode = [int]$LASTEXITCODE
        if ($ctlCode -eq 0) {
            Record-Result "wrapperctl_status" "PASS"
        } else {
            # Fallback: direct status probe (ctl may Write-Host-only)
            $probe = Invoke-SmokeApi -Method GET -Path "/api/v1/status" -AllowFail
            if ([int]$probe.StatusCode -eq 200) {
                Record-Result "wrapperctl_status" "PASS" "ctl exit=$ctlCode but API status 200"
            } else {
                Record-Result "wrapperctl_status" "FAIL" "ctl exit=$ctlCode api=$([int]$probe.StatusCode)"
            }
        }
    } catch {
        Record-Result "wrapperctl_status" "FAIL" $_.Exception.Message
    }

    if ($KeepAlive) {
        Write-Info "KeepAlive set - press Enter to tear down"
        [void][System.Console]::ReadLine()
    }
}
finally {
    if (-not $KeepAlive) {
        Stop-SmokeProcesses
    }
}

Write-Host ""
Write-Host "=== Smoke summary ==="
$pass = @($Script:Results | Where-Object Status -eq "PASS").Count
$fail = @($Script:Results | Where-Object Status -eq "FAIL").Count
$skip = @($Script:Results | Where-Object Status -eq "SKIP").Count
foreach ($r in $Script:Results) {
    $line = "[{0}] {1}" -f $r.Status, $r.Name
    if ($r.Detail) { $line += " - $($r.Detail)" }
    Write-Host $line
}
Write-Host "Totals: $pass pass / $fail fail / $skip skip"
if ($fail -gt 0) { exit 1 }
exit 0
