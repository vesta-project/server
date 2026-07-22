#Requires -Version 5.1
<#
.SYNOPSIS
  Multi-instance control CLI for Vesta wrapper installs.

.DESCRIPTION
  Reads identity from %USERPROFILE%\.vesta\instances.json and resolves HTTP/control
  ports from each instance's wrapper-config.toml at runtime (ports are never stored
  in the registry).

.EXAMPLE
  .\scripts\vesta-wrapperctl.ps1 list
  .\scripts\vesta-wrapperctl.ps1 status --instance my-paper-server
  .\scripts\vesta-wrapperctl.ps1 api GET /api/v1/status --config C:\servers\paper\wrapper-config.toml
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Command,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ArgsRest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Write-Info([string]$Message) { Write-Host "[vesta-wrapperctl] $Message" }
function Write-WarnMsg([string]$Message) { Write-Warning $Message }
function Die([string]$Message) {
    Write-Host "error: $Message" -ForegroundColor Red
    exit 1
}

function Show-Usage {
    @"
Usage: vesta-wrapperctl.ps1 <command> [options]

Commands:
  list                              Show registered instances
  discover --roots <dir[,dir...]>   Scan for wrapper-config.toml
                                    (optional --register to upsert finds)
  register --config <path> [--name] Manual registry add/upsert
  unregister --config|--instance    Remove registry entry
  status                            Config ports + GET /api/v1/status
  start | stop | restart            OS service (registry service_name), else start script / process
  logs [--lines N] [--follow]       Tail .vesta/*.log or console API
  api <METHOD> <path> [body]        Local HTTPS client (curl-like)
  devtools                          Print loopback devtools URL
  uninstall [--delete-files] [--yes]
                                    Stop service, unregister, remove wrapper artifacts
                                    (keep server files unless --delete-files)

Instance selectors (precedence):
  --config <path> > --instance <name> > --http-port <n> > single registered instance

Auth (for logs / protected api):
  --token <bearer>   or env VESTA_TOKEN
  --password <pass>  (POST /api/v1/auth/token when mode=password)

Other:
  --yes / -y         Non-interactive confirmations
  -h / --help        This help
"@ | Write-Host
}

# --- arg parsing -------------------------------------------------------------

$Script:Opt = [ordered]@{
    Config      = $null
    Instance    = $null
    HttpPort    = $null
    Name        = $null
    Roots       = $null
    Register    = $false
    Lines       = 100
    Follow      = $false
    DeleteFiles = $false
    Yes         = $false
    Token       = $env:VESTA_TOKEN
    Password    = $null
    Positional  = New-Object System.Collections.Generic.List[string]
}

function Parse-Args([string[]]$argv) {
    if ($null -eq $argv) { $argv = @() }
    $i = 0
    while ($i -lt @($argv).Count) {
        $a = $argv[$i]
        switch -Regex ($a) {
            "^(--help|-h)$" { Show-Usage; exit 0 }
            "^--config$" {
                $i++; if ($i -ge $argv.Count) { Die "--config requires a path" }
                $Script:Opt.Config = $argv[$i]
            }
            "^--instance$" {
                $i++; if ($i -ge $argv.Count) { Die "--instance requires a name" }
                $Script:Opt.Instance = $argv[$i]
            }
            "^--http-port$" {
                $i++; if ($i -ge $argv.Count) { Die "--http-port requires a number" }
                $Script:Opt.HttpPort = [int]$argv[$i]
            }
            "^--name$" {
                $i++; if ($i -ge $argv.Count) { Die "--name requires a value" }
                $Script:Opt.Name = $argv[$i]
            }
            "^--roots$" {
                $i++; if ($i -ge $argv.Count) { Die "--roots requires a value" }
                $Script:Opt.Roots = $argv[$i]
            }
            "^--register$" { $Script:Opt.Register = $true }
            "^--lines$" {
                $i++; if ($i -ge $argv.Count) { Die "--lines requires a number" }
                $Script:Opt.Lines = [int]$argv[$i]
            }
            "^--follow$" { $Script:Opt.Follow = $true }
            "^--delete-files$" { $Script:Opt.DeleteFiles = $true }
            "^(--yes|-y)$" { $Script:Opt.Yes = $true }
            "^--token$" {
                $i++; if ($i -ge $argv.Count) { Die "--token requires a value" }
                $Script:Opt.Token = $argv[$i]
            }
            "^--password$" {
                $i++; if ($i -ge $argv.Count) { Die "--password requires a value" }
                $Script:Opt.Password = $argv[$i]
            }
            default { [void]$Script:Opt.Positional.Add($a) }
        }
        $i++
    }
}

# --- registry / config -------------------------------------------------------

function Get-RegistryPath {
    Join-Path (Join-Path $env:USERPROFILE ".vesta") "instances.json"
}

function Read-Registry {
    $path = Get-RegistryPath
    if (-not (Test-Path $path)) {
        return [pscustomobject]@{ version = 1; instances = @() }
    }
    $raw = Get-Content $path -Raw -ErrorAction Stop
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return [pscustomobject]@{ version = 1; instances = @() }
    }
    $data = $raw | ConvertFrom-Json
    # ConvertFrom-Json unwraps single-element arrays — normalize to Object[].
    $inst = @()
    if ($null -ne $data.instances) { $inst = @($data.instances) }
    $version = 1
    if ($null -ne $data.version) { $version = [int]$data.version }
    return [pscustomobject]@{ version = $version; instances = $inst }
}

function Write-Registry($data) {
    $path = Get-RegistryPath
    $dir = Split-Path $path -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $tmp = "$path.tmp"
    # Force instances to stay a JSON array even when count is 0 or 1.
    $payload = [ordered]@{
        version   = if ($null -ne $data.version) { [int]$data.version } else { 1 }
        instances = @($data.instances)
    }
    $json = ($payload | ConvertTo-Json -Depth 20)
    # ConvertTo-Json still unwraps single-element arrays in Windows PowerShell —
    # rewrite instances via .NET/manual guard when needed.
    if (@($payload.instances).Count -eq 1) {
        $one = ($payload.instances[0] | ConvertTo-Json -Depth 20 -Compress)
        $json = "{`r`n  `"version`": $($payload.version),`r`n  `"instances`": [`r`n    $one`r`n  ]`r`n}"
    } elseif (@($payload.instances).Count -eq 0) {
        $json = "{`r`n  `"version`": $($payload.version),`r`n  `"instances`": []`r`n}"
    }
    Set-Content -Path $tmp -Value $json -Encoding UTF8
    Move-Item -Path $tmp -Destination $path -Force
}

function Slugify([string]$Value) {
    $s = $Value.ToLowerInvariant()
    $s = $s -replace "[^a-z0-9]+", "-"
    $s = $s -replace "^-+", ""
    $s = $s -replace "-+$", ""
    $s = $s -replace "-{2,}", "-"
    if ([string]::IsNullOrWhiteSpace($s)) { $s = "vesta-server" }
    return $s
}

function Resolve-AbsPath([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) { return $null }
    if (Test-Path -LiteralPath $Path) {
        return (Resolve-Path -LiteralPath $Path).Path
    }
    # Path may not exist - still canonicalize relative to cwd
    return [IO.Path]::GetFullPath($Path)
}

function Read-WrapperConfig([string]$ConfigPath) {
    $out = [ordered]@{
        HttpPort      = $null
        ControlPort   = $null
        IdentityId    = $null
        IdentityName  = $null
        TlsEnabled    = $true
        Scheme        = "https"
        PublicHost    = "localhost"
        PublicPort    = $null
        SecurityMode  = "open"
        SecurityPassword = ""
        RootHint      = $null
    }
    if (-not (Test-Path -LiteralPath $ConfigPath)) { return [pscustomobject]$out }

    $section = ""
    foreach ($line in (Get-Content -LiteralPath $ConfigPath)) {
        if ($line -match "^\s*#") { continue }
        if ($line -match "^\s*\[(.+)\]\s*$") {
            $section = $matches[1].Trim()
            continue
        }
        switch ($section) {
            "server" {
                if ($line -match "^\s*http-port\s*=\s*(\d+)\s*$") { $out.HttpPort = [int]$matches[1] }
                elseif ($line -match "^\s*tcp-port\s*=\s*(\d+)\s*$" -and $null -eq $out.HttpPort) {
                    # legacy alias; prefer http-port when both present
                    $out.HttpPort = [int]$matches[1]
                }
            }
            "server.public" {
                if ($line -match "^\s*scheme\s*=\s*""?([^""#]+)""?\s*$") { $out.Scheme = $matches[1].Trim() }
                elseif ($line -match "^\s*host\s*=\s*""?([^""#]+)""?\s*$") { $out.PublicHost = $matches[1].Trim() }
                elseif ($line -match "^\s*port\s*=\s*(\d+)\s*$") { $out.PublicPort = [int]$matches[1] }
            }
            "server.tls" {
                if ($line -match "^\s*enabled\s*=\s*(true|false)\s*$") {
                    $out.TlsEnabled = ($matches[1] -eq "true")
                    if (-not $out.TlsEnabled) { $out.Scheme = "http" }
                }
            }
            "server.identity" {
                if ($line -match "^\s*id\s*=\s*""?([^""#]+)""?\s*$") { $out.IdentityId = $matches[1].Trim() }
                elseif ($line -match "^\s*name\s*=\s*""?([^""#]+)""?\s*$") { $out.IdentityName = $matches[1].Trim() }
            }
            "security" {
                if ($line -match "^\s*control-port\s*=\s*(\d+)\s*$") { $out.ControlPort = [int]$matches[1] }
                elseif ($line -match "^\s*mode\s*=\s*""?([^""#]+)""?\s*$") { $out.SecurityMode = $matches[1].Trim() }
                elseif ($line -match "^\s*password\s*=\s*""?([^""#]*)""?\s*$") { $out.SecurityPassword = $matches[1] }
            }
        }
    }

    # Prefer http-port; never invent 9090 as the sole source - leave null if missing.
    if ($null -eq $out.HttpPort -and $null -ne $out.PublicPort) {
        $out.HttpPort = $out.PublicPort
    }
    if ($out.TlsEnabled -eq $false) { $out.Scheme = "http" }
    elseif ($out.Scheme -ne "http" -and $out.Scheme -ne "https") {
        $out.Scheme = if ($out.TlsEnabled) { "https" } else { "http" }
    }

    return [pscustomobject]$out
}

function Get-BaseUrl($cfg) {
    if ($null -eq $cfg.HttpPort -or $cfg.HttpPort -le 0) {
        Die "wrapper-config.toml has no http-port (and no public.port fallback). Refusing to guess."
    }
    $hostName = if ($cfg.PublicHost) { $cfg.PublicHost } else { "localhost" }
    # Always prefer loopback for local ops
    if ($hostName -eq "0.0.0.0" -or $hostName -eq "*" -or $hostName -eq "::") {
        $hostName = "localhost"
    }
    return ("{0}://{1}:{2}" -f $cfg.Scheme, $hostName, $cfg.HttpPort)
}

function Find-InstancesByHttpPort([int]$Port) {
    $data = Read-Registry
    $matches = @()
    foreach ($inst in @($data.instances)) {
        if (-not $inst.config -or -not (Test-Path -LiteralPath $inst.config)) { continue }
        $cfg = Read-WrapperConfig $inst.config
        if ($cfg.HttpPort -eq $Port) { $matches += $inst }
    }
    return $matches
}

function Resolve-TargetInstance {
    # Precedence: --config > --instance > --http-port > single registered
    $data = Read-Registry
    $instances = @($data.instances)

    if ($Script:Opt.Config) {
        $abs = Resolve-AbsPath $Script:Opt.Config
        if (-not (Test-Path -LiteralPath $abs)) {
            Die "Config not found: $abs"
        }
        $hit = $instances | Where-Object { (Resolve-AbsPath $_.config) -eq $abs } | Select-Object -First 1
        if ($hit) { return $hit }
        # Ad-hoc target not in registry
        $root = Split-Path $abs -Parent
        $slug = if ($Script:Opt.Name) {
            Slugify $Script:Opt.Name
        } elseif ($Script:Opt.Instance) {
            Slugify $Script:Opt.Instance
        } else {
            Slugify ([IO.Path]::GetFileName($root))
        }
        return [pscustomobject]@{
            name         = $slug
            config       = $abs
            root         = $root
            service_name = "vesta-wrapper-$slug"
            registered   = $false
        }
    }

    if ($Script:Opt.Instance) {
        $hit = $instances | Where-Object { $_.name -eq $Script:Opt.Instance } | Select-Object -First 1
        if (-not $hit) { Die "No registered instance named '$($Script:Opt.Instance)'. Run: vesta-wrapperctl list" }
        return $hit
    }

    if ($null -ne $Script:Opt.HttpPort) {
        $hits = @(Find-InstancesByHttpPort $Script:Opt.HttpPort)
        if ($hits.Count -eq 0) {
            Die "No registered instance with http-port=$($Script:Opt.HttpPort) in its wrapper-config.toml. Run: vesta-wrapperctl list"
        }
        if ($hits.Count -gt 1) {
            $names = ($hits | ForEach-Object { $_.name }) -join ", "
            Die "Multiple instances share http-port=$($Script:Opt.HttpPort): $names. Use --config or --instance."
        }
        return $hits[0]
    }

    if ($instances.Count -eq 1) { return $instances[0] }
    if ($instances.Count -eq 0) {
        Die 'No registered instances. Use --config PATH, register one, or run: vesta-wrapperctl list'
    }
    Die "Multiple registered instances; specify --config, --instance, or --http-port. Hint: vesta-wrapperctl list"
}

# --- HTTP helpers ------------------------------------------------------------

function Disable-CertValidation {
    # Allow self-signed installer TLS material for local ops.
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
    try {
        if (-not ([System.Management.Automation.PSTypeName]"TrustAllCertsPolicy").Type) {
            Add-Type @"
using System.Net;
using System.Security.Cryptography.X509Certificates;
public class TrustAllCertsPolicy : ICertificatePolicy {
  public bool CheckValidationResult(ServicePoint s, X509Certificate c, WebRequest r, int p) { return true; }
}
"@
        }
        [System.Net.ServicePointManager]::CertificatePolicy = New-Object TrustAllCertsPolicy
    } catch {}
    try {
        [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
    } catch {}
}

function Invoke-VestaApi {
    param(
        [string]$Method,
        [string]$Url,
        [string]$Body = $null,
        [string]$Token = $null,
        [switch]$Raw,
        [switch]$AllowFail
    )
    Disable-CertValidation
    $headers = @{ }
    if ($Token) { $headers["Authorization"] = "Bearer $Token" }
    $params = @{
        Method      = $Method
        Uri         = $Url
        Headers     = $headers
        TimeoutSec  = 10
        ErrorAction = "Stop"
    }
    if ($null -ne $Body -and $Method -notin @("GET", "HEAD")) {
        $params.ContentType = "application/json"
        $params.Body = $Body
    }
    try {
        if ($Raw) {
            return Invoke-WebRequest @params -UseBasicParsing
        }
        return Invoke-RestMethod @params -UseBasicParsing
    } catch {
        if ($AllowFail) { return $null }
        $resp = $_.Exception.Response
        if ($resp) {
            $code = [int]$resp.StatusCode
            $reader = New-Object IO.StreamReader($resp.GetResponseStream())
            $text = $reader.ReadToEnd()
            Die "HTTP $code $Method $Url`n$text"
        }
        Die "Request failed: $Method $Url - $($_.Exception.Message)"
    }
}

function Ensure-Token($cfg, [string]$BaseUrl) {
    if ($Script:Opt.Token) { return $Script:Opt.Token }
    $password = $Script:Opt.Password
    if (-not $password -and $cfg.SecurityMode -eq "password" -and $cfg.SecurityPassword) {
        $password = $cfg.SecurityPassword
    }
    if ($password) {
        $body = (@{ password = $password } | ConvertTo-Json -Compress)
        $resp = Invoke-VestaApi -Method POST -Url "$BaseUrl/api/v1/auth/token" -Body $body
        if ($resp.token) { return [string]$resp.token }
    }
    return $null
}

# --- service / process -------------------------------------------------------

function Test-ScheduledTaskExists([string]$Name) {
    try {
        $null = Get-ScheduledTask -TaskName $Name -ErrorAction Stop
        return $true
    } catch { return $false }
}

function Start-InstanceService($inst) {
    $svc = [string]$inst.service_name
    if ($svc -and (Test-ScheduledTaskExists $svc)) {
        Write-Info "Starting scheduled task '$svc'"
        Start-ScheduledTask -TaskName $svc -ErrorAction Stop
        return $true
    }
    return $false
}

function Stop-InstanceService($inst) {
    $svc = [string]$inst.service_name
    if ($svc -and (Test-ScheduledTaskExists $svc)) {
        Write-Info "Stopping scheduled task '$svc'"
        try { Stop-ScheduledTask -TaskName $svc -ErrorAction SilentlyContinue } catch {}
        # Also end via schtasks for broader compatibility
        & schtasks.exe /End /TN $svc 2>$null | Out-Null
        return $true
    }
    return $false
}

function Remove-InstanceService($inst) {
    $svc = [string]$inst.service_name
    if ($svc -and (Test-ScheduledTaskExists $svc)) {
        Write-Info "Unregistering scheduled task '$svc'"
        Unregister-ScheduledTask -TaskName $svc -Confirm:$false -ErrorAction SilentlyContinue
        return $true
    }
    return $false
}

function Start-ManualWrapper($inst) {
    $root = [string]$inst.root
    $bat = Join-Path $root "start-vesta-wrapper.bat"
    $wrapperBat = Join-Path $root "vesta-wrapper\bin\wrapper.bat"
    $config = [string]$inst.config
    if (Test-Path -LiteralPath $bat) {
        Write-Info "Starting via $bat"
        Start-Process -FilePath $bat -WorkingDirectory $root -WindowStyle Hidden | Out-Null
        return
    }
    if (Test-Path -LiteralPath $wrapperBat) {
        Write-Info "Starting via $wrapperBat --config $config"
        Start-Process -FilePath $wrapperBat -ArgumentList @("--config", $config) -WorkingDirectory $root -WindowStyle Hidden | Out-Null
        return
    }
    Die "No scheduled task '$($inst.service_name)' and no start-vesta-wrapper.bat / vesta-wrapper\\bin\\wrapper.bat under $root"
}

function Stop-ManualWrapper($inst) {
    $config = [string]$inst.config
    $escaped = [Regex]::Escape($config)
    $procs = Get-CimInstance Win32_Process -Filter "Name = 'java.exe' OR Name = 'javaw.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and ($_.CommandLine -match $escaped -or $_.CommandLine -match "vesta-wrapper") }
    # Narrow to config path when possible
    $hits = @($procs | Where-Object { $_.CommandLine -match $escaped })
    if ($hits.Count -eq 0) { $hits = @($procs) }
    if ($hits.Count -eq 0) {
        Write-WarnMsg "No matching Java process found for config $config"
        return
    }
    foreach ($p in $hits) {
        Write-Info "Stopping PID $($p.ProcessId)"
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
}

function Wait-StatusReady([string]$BaseUrl, [int]$Attempts = 20) {
    for ($i = 0; $i -lt $Attempts; $i++) {
        $resp = Invoke-VestaApi -Method GET -Url "$BaseUrl/api/v1/status" -AllowFail
        if ($null -ne $resp) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

# --- commands ----------------------------------------------------------------

function Cmd-List {
    $data = Read-Registry
    $instances = @($data.instances)
    if ($instances.Count -eq 0) {
        Write-Host "No registered instances ($((Get-RegistryPath)))."
        Write-Host 'Hint: vesta-wrapperctl discover --roots DIR --register'
        return
    }
    Write-Host ("{0,-24} {1,-8} {2,-10} {3}" -f "NAME", "HTTP", "SERVICE", "CONFIG")
    Write-Host ("{0,-24} {1,-8} {2,-10} {3}" -f ("-" * 24), ("-" * 8), ("-" * 10), ("-" * 40))
    foreach ($inst in $instances) {
        $http = "?"
        if ($inst.config -and (Test-Path -LiteralPath $inst.config)) {
            $cfg = Read-WrapperConfig $inst.config
            if ($null -ne $cfg.HttpPort) { $http = [string]$cfg.HttpPort }
        }
        Write-Host ("{0,-24} {1,-8} {2,-10} {3}" -f $inst.name, $http, $inst.service_name, $inst.config)
    }
}

function Cmd-Discover {
    if (-not $Script:Opt.Roots) { Die 'discover requires --roots DIR[,DIR...]' }
    $roots = $Script:Opt.Roots -split '[,;]' | ForEach-Object { $_.Trim() } | Where-Object { $_ }
    $found = @()
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) {
            Write-WarnMsg "Root not found: $root"
            continue
        }
        Get-ChildItem -LiteralPath $root -Filter "wrapper-config.toml" -Recurse -File -ErrorAction SilentlyContinue |
            ForEach-Object {
                $cfgPath = $_.FullName
                $serverRoot = $_.Directory.FullName
                $cfg = Read-WrapperConfig $cfgPath
                $obj = [pscustomobject]@{
                    config     = $cfgPath
                    root       = $serverRoot
                    http_port  = $cfg.HttpPort
                    identity   = $cfg.IdentityId
                }
                $found += $obj
                Write-Host ("FOUND  http={0,-6} {1}" -f $(if ($null -ne $cfg.HttpPort) { $cfg.HttpPort } else { "?" }), $cfgPath)
                if ($Script:Opt.Register) {
                    $name = if ($Script:Opt.Name) { Slugify $Script:Opt.Name } else { Slugify ([IO.Path]::GetFileName($serverRoot)) }
                    Registry-UpsertLocal -InstanceName $name -ConfigPath $cfgPath -RootPath $serverRoot -ServerId $cfg.IdentityId -ServiceName ("vesta-wrapper-$name")
                    Write-Info "Registered as '$name'"
                }
            }
    }
    if ($found.Count -eq 0) {
        Write-Host "No wrapper-config.toml found under: $($roots -join ', ')"
    }
}

function Registry-UpsertLocal {
    param(
        [string]$InstanceName,
        [string]$ConfigPath,
        [string]$RootPath,
        [string]$ServerId,
        [string]$ServiceName
    )
    $data = Read-Registry
    $canonical = Resolve-AbsPath $ConfigPath
    $rootAbs = Resolve-AbsPath $RootPath
    $now = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $existing = $null
    foreach ($inst in @($data.instances)) {
        if ((Resolve-AbsPath $inst.config) -eq $canonical) { $existing = $inst; break }
    }
    if ($existing) {
        $existing.name = $InstanceName
        $existing.root = $rootAbs
        $existing.service_name = $ServiceName
        if ($ServerId) { $existing.server_id = $ServerId }
        try { $existing.PSObject.Properties.Remove("http_port") } catch {}
        try { $existing.PSObject.Properties.Remove("control_port") } catch {}
    } else {
        $newInst = [pscustomobject]@{
            name          = $InstanceName
            config        = $canonical
            root          = $rootAbs
            service_name  = $ServiceName
            registered_at = $now
        }
        if ($ServerId) {
            $newInst | Add-Member -NotePropertyName server_id -NotePropertyValue $ServerId
        }
        $data.instances = @($data.instances) + @($newInst)
    }
    Write-Registry $data
}

function Cmd-Register {
    if (-not $Script:Opt.Config) { Die 'register requires --config PATH' }
    $abs = Resolve-AbsPath $Script:Opt.Config
    if (-not (Test-Path -LiteralPath $abs)) { Die "Config not found: $abs" }
    $root = Split-Path $abs -Parent
    $name = if ($Script:Opt.Name) { Slugify $Script:Opt.Name } else { Slugify ([IO.Path]::GetFileName($root)) }
    $cfg = Read-WrapperConfig $abs
    $svc = "vesta-wrapper-$name"
    Registry-UpsertLocal -InstanceName $name -ConfigPath $abs -RootPath $root -ServerId $cfg.IdentityId -ServiceName $svc
    Write-Info "Registered '$name' -> $abs (service_name=$svc)"
}

function Cmd-Unregister {
    $data = Read-Registry
    $canonical = $null
    $nameFilter = $null
    if ($Script:Opt.Config) {
        $canonical = Resolve-AbsPath $Script:Opt.Config
    } elseif ($Script:Opt.Instance) {
        $nameFilter = $Script:Opt.Instance
    } else {
        Die "unregister requires --config or --instance"
    }
    $kept = @()
    $removed = 0
    foreach ($inst in @($data.instances)) {
        $drop = $false
        if ($canonical -and ((Resolve-AbsPath $inst.config) -eq $canonical)) { $drop = $true }
        if ($nameFilter -and $inst.name -eq $nameFilter) { $drop = $true }
        if ($drop) { $removed++; Write-Info "Unregistered '$($inst.name)' ($($inst.config))" }
        else { $kept += $inst }
    }
    if ($removed -eq 0) { Die "No matching registry entry." }
    $data.instances = $kept
    Write-Registry $data
}

function Cmd-Status {
    $inst = Resolve-TargetInstance
    $cfg = Read-WrapperConfig $inst.config
    $base = $null
    $httpOk = $false
    $statusObj = $null
    $base = Get-BaseUrl $cfg
    $statusObj = Invoke-VestaApi -Method GET -Url "$base/api/v1/status" -AllowFail
    if ($null -ne $statusObj) { $httpOk = $true }

    $svc = [string]$inst.service_name
    $svcState = "absent"
    if ($svc -and (Test-ScheduledTaskExists $svc)) {
        try {
            $info = Get-ScheduledTaskInfo -TaskName $svc
            $svcState = [string]$info.LastTaskResult
            $task = Get-ScheduledTask -TaskName $svc
            $svcState = "$($task.State) (lastResult=$($info.LastTaskResult))"
        } catch { $svcState = "present" }
    }

    Write-Host "instance:     $($inst.name)"
    Write-Host "config:       $($inst.config)"
    Write-Host "root:         $($inst.root)"
    Write-Host "service_name: $svc  [$svcState]"
    Write-Host ("http-port:    {0}" -f $(if ($null -ne $cfg.HttpPort) { $cfg.HttpPort } else { "(unset)" }))
    Write-Host ("control-port: {0}" -f $(if ($null -ne $cfg.ControlPort) { $cfg.ControlPort } else { "(unset)" }))
    if ($base) { Write-Host "base-url:     $base" }
    if ($httpOk) {
        Write-Host "api-status:   up"
        ($statusObj | ConvertTo-Json -Depth 8) | Write-Host
    } else {
        Write-Host "api-status:   down (or unreachable)"
        exit 2
    }
}

function Cmd-Start {
    $inst = Resolve-TargetInstance
    if (-not (Start-InstanceService $inst)) {
        Start-ManualWrapper $inst
    }
    $cfg = Read-WrapperConfig $inst.config
    try {
        $base = Get-BaseUrl $cfg
        if (Wait-StatusReady $base) {
            Write-Info "Wrapper is up at $base"
        } else {
            Write-WarnMsg "Started, but /api/v1/status not ready yet ($base)"
        }
    } catch {
        Write-WarnMsg $_.Exception.Message
    }
}

function Cmd-Stop {
    $inst = Resolve-TargetInstance
    $cfg = Read-WrapperConfig $inst.config
    $usedService = Stop-InstanceService $inst

    # If wrapper is still reachable, try Minecraft power stop (local admin / capability).
    try {
        $base = Get-BaseUrl $cfg
        $token = Ensure-Token $cfg $base
        $body = '{"action":"stop"}'
        $null = Invoke-VestaApi -Method POST -Url "$base/api/v1/server/power" -Body $body -Token $token -AllowFail
    } catch {}

    if (-not $usedService) {
        Stop-ManualWrapper $inst
    } else {
        # Ensure process is gone even if task end left orphans
        Start-Sleep -Milliseconds 400
        Stop-ManualWrapper $inst
    }
    Write-Info "Stop complete for '$($inst.name)'"
}

function Cmd-Restart {
    Cmd-Stop
    Start-Sleep -Seconds 1
    Cmd-Start
}

function Cmd-Logs {
    $inst = Resolve-TargetInstance
    $root = [string]$inst.root
    $candidates = @(
        (Join-Path $root ".vesta\wrapper.out.log"),
        (Join-Path $root ".vesta\wrapper.err.log"),
        (Join-Path $root ".vesta\wrapper.log"),
        (Join-Path $root "logs\latest.log")
    )
    $logFile = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if ($logFile) {
        Write-Info "Tailing $logFile (last $($Script:Opt.Lines) lines)"
        if ($Script:Opt.Follow) {
            Get-Content -LiteralPath $logFile -Tail $Script:Opt.Lines -Wait
        } else {
            Get-Content -LiteralPath $logFile -Tail $Script:Opt.Lines
        }
        return
    }

    Write-Info "No local log file; trying GET /api/v1/console (needs console.read bearer)"
    $cfg = Read-WrapperConfig $inst.config
    $base = Get-BaseUrl $cfg
    $token = Ensure-Token $cfg $base
    if (-not $token) {
        Die "No log file under $root\.vesta and no --token/VESTA_TOKEN (or --password). Console API requires a bearer with console.read."
    }
    $limit = [Math]::Max(1, [int]$Script:Opt.Lines)
    $url = "$base/api/v1/console?since=0&limit=$limit"
    $resp = Invoke-VestaApi -Method GET -Url $url -Token $token
    if ($resp.lines) {
        foreach ($line in $resp.lines) {
            if ($line.text) { Write-Host $line.text }
            elseif ($line.line) { Write-Host $line.line }
            else { Write-Host ($line | ConvertTo-Json -Compress) }
        }
    } else {
        ($resp | ConvertTo-Json -Depth 6) | Write-Host
    }
    if ($Script:Opt.Follow) {
        Write-WarnMsg "--follow via API polls every 1s (Ctrl+C to stop)"
        $since = 0
        if ($resp.cursor) { $since = [int64]$resp.cursor }
        while ($true) {
            Start-Sleep -Seconds 1
            $url = "$base/api/v1/console?since=$since&limit=$limit"
            $resp = Invoke-VestaApi -Method GET -Url $url -Token $token
            if ($resp.lines) {
                foreach ($line in $resp.lines) {
                    if ($line.text) { Write-Host $line.text }
                    elseif ($line.line) { Write-Host $line.line }
                }
            }
            if ($resp.cursor) { $since = [int64]$resp.cursor }
        }
    }
}

function Cmd-Api {
    $pos = @($Script:Opt.Positional)
    if ($pos.Count -lt 2) { Die 'api requires: api METHOD PATH [body]' }
    $method = $pos[0].ToUpperInvariant()
    $path = $pos[1]
    $body = if ($pos.Count -ge 3) { ($pos[2..($pos.Count - 1)] -join " ") } else { $null }
    if (-not $path.StartsWith("/")) { $path = "/$path" }

    $inst = Resolve-TargetInstance
    $cfg = Read-WrapperConfig $inst.config
    $base = Get-BaseUrl $cfg
    $token = Ensure-Token $cfg $base
    $url = "$base$path"
    Write-Info "$method $url"
    $resp = Invoke-VestaApi -Method $method -Url $url -Body $body -Token $token -Raw
    Write-Host "HTTP $([int]$resp.StatusCode)"
    if ($resp.Content) { Write-Host $resp.Content }
}

function Cmd-Devtools {
    $inst = Resolve-TargetInstance
    $cfg = Read-WrapperConfig $inst.config
    $base = Get-BaseUrl $cfg
    Write-Host "$base/devtools"
    Write-Host "(loopback-only debug UI - not for production ops)"
}

function Cmd-Uninstall {
    $inst = Resolve-TargetInstance
    $root = [string]$inst.root
    $name = [string]$inst.name

    if ($Script:Opt.DeleteFiles -and -not $Script:Opt.Yes) {
        $ans = Read-Host ("DELETE entire server root {0} for instance {1}? Type YES to confirm" -f $root, $name)
        if ($ans -ne "YES") { Die "Aborted." }
    } elseif (-not $Script:Opt.DeleteFiles -and -not $Script:Opt.Yes) {
        $ans = Read-Host ("Uninstall wrapper for {0} (keep Minecraft/server files under {1})? [y/N]" -f $name, $root)
        if (-not ($ans -and $ans.ToLowerInvariant().StartsWith("y"))) { Die "Aborted." }
    }

    Write-Info "Stopping '$name'..."
    try { $null = Stop-InstanceService $inst } catch {}
    try { Stop-ManualWrapper $inst } catch {}

    try { Remove-InstanceService $inst } catch {}

    # Remove registry entry by config path
    $data = Read-Registry
    $abs = Resolve-AbsPath $inst.config
    $data.instances = @($data.instances | Where-Object { (Resolve-AbsPath $_.config) -ne $abs })
    Write-Registry $data
    Write-Info "Removed registry entry"

    if ($Script:Opt.DeleteFiles) {
        Write-Info "Deleting server root $root"
        if (Test-Path -LiteralPath $root) {
            Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction Stop
        }
        Write-Info "Uninstall complete (files deleted)."
        return
    }

    # Keep server files; remove wrapper artifacts only
    $artifacts = @(
        (Join-Path $root "vesta-wrapper"),
        (Join-Path $root "start-vesta-wrapper.bat"),
        (Join-Path $root "start-vesta-wrapper.sh"),
        (Join-Path $root ".vesta\wrapper.out.log"),
        (Join-Path $root ".vesta\wrapper.err.log")
    )
    foreach ($p in $artifacts) {
        if (Test-Path -LiteralPath $p) {
            Write-Info "Removing $p"
            Remove-Item -LiteralPath $p -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    Write-Info 'Uninstall complete (server files kept; wrapper-config.toml retained).'
    Write-Host "Hint: delete wrapper-config.toml manually if you no longer need it."
}

# --- main --------------------------------------------------------------------

if ([string]::IsNullOrWhiteSpace($Command) -or $Command -in @("-h", "--help", "help")) {
    Show-Usage
    exit 0
}

if ($null -eq $ArgsRest) { $ArgsRest = @() }
Parse-Args $ArgsRest

switch ($Command.ToLowerInvariant()) {
    "list"       { Cmd-List }
    "discover"   { Cmd-Discover }
    "register"   { Cmd-Register }
    "unregister" { Cmd-Unregister }
    "status"     { Cmd-Status }
    "start"      { Cmd-Start }
    "stop"       { Cmd-Stop }
    "restart"    { Cmd-Restart }
    "logs"       { Cmd-Logs }
    "api"        { Cmd-Api }
    "devtools"   { Cmd-Devtools }
    "uninstall"  { Cmd-Uninstall }
    default {
        Die "Unknown command '$Command'. Run with --help."
    }
}
