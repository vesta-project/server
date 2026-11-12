#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Root = (Join-Path (Split-Path -Parent $PSScriptRoot) '.dev\fixture'),
    [ValidateRange(1,65535)][int]$Port = 8443,
    [ValidateRange(1,65535)][int]$GamePort = 25565,
    [string]$Version = 'latest',
    [string]$Java = '',
    [ValidateSet('vanilla','fabric')][string]$Loader = 'vanilla',
    [string]$LoaderVersion = '',
    [switch]$AcceptEula,
    [switch]$Inert,
    [switch]$Release,
    [switch]$Background,
    [switch]$Reset,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = Split-Path -Parent $PSScriptRoot
$Root = [IO.Path]::GetFullPath($Root).TrimEnd('\','/')
$marker = Join-Path $Root '.vesta-dev-server'
$name = 'dev-server'
$utf8 = New-Object Text.UTF8Encoding($false)
function Write-Utf8([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path,$Text,$utf8) }
function Invoke-Program([string]$Executable, [string[]]$Arguments) {
    # PowerShell 5.1 turns native stderr into ErrorRecords when output is redirected.
    # Cargo progress is normal stderr; determine native failure from the exit code.
    $ErrorActionPreference = 'Continue'
    & $Executable @Arguments 2>&1 | ForEach-Object {
        if ($_ -is [Management.Automation.ErrorRecord]) { Write-Host $_.ToString() }
        else { Write-Output $_ }
    }
    if ($LASTEXITCODE -ne 0) { throw "$Executable failed ($LASTEXITCODE). See output above." }
}
function Run-Agent([string[]]$Arguments) {
    Invoke-Program -Executable $agent -Arguments $Arguments
}
function Agent-Ready {
    $ErrorActionPreference = 'Continue'
    & $agent access show $name --install-root $installRoot 2>$null | Out-Null
    return ($LASTEXITCODE -eq 0)
}
function Toml-String([string]$Value) { return (ConvertTo-Json -InputObject $Value.Replace('\','/') -Compress) }
# A reset is restricted to a marked scratch directory, never a drive root, checkout,
# ancestor of the checkout, or reparse point. Resolve and check before deletion.
if ($Root -eq [IO.Path]::GetPathRoot($Root).TrimEnd('\','/') -or
    $repo.StartsWith($Root + '\',[StringComparison]::OrdinalIgnoreCase) -or $Root -eq $repo) {
    throw 'Root must be a dedicated scratch directory, outside the repository ancestors.'
}
$ancestor = $Root
while ($ancestor) {
    if (Test-Path -LiteralPath $ancestor) {
        if ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Scratch path contains a reparse point: $ancestor"
        }
    }
    $parent = Split-Path -Parent $ancestor
    if ($parent -eq $ancestor) { break }
    $ancestor = $parent
}
if ($Reset) {
    if (Test-Path -LiteralPath $Root) {
        if (!(Test-Path -LiteralPath $marker) -or [IO.File]::ReadAllText($marker).Trim() -ne 'vesta-dev-server-v1') {
            throw 'Reset refused: this directory is not a marked Vesta dev server.'
        }
        if (Test-Path -LiteralPath (Join-Path $Root 'agent.pid')) {
            $runningId = [int][IO.File]::ReadAllText((Join-Path $Root 'agent.pid'))
            if (Get-Process -Id $runningId -ErrorAction SilentlyContinue) { throw "Stop the dev server (PID $runningId) before resetting." }
        }
        # Refuse any linked child so recursive removal cannot reach outside scratch.
        $linked = Get-ChildItem -LiteralPath $Root -Force -Recurse | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint } | Select-Object -First 1
        if ($linked) { throw "Reset refused: linked child $($linked.FullName)" }
        if (!$Force -and (Read-Host "Delete all disposable data at '$Root'? Type DELETE") -cne 'DELETE') {
            Write-Host 'Reset cancelled.'; return
        }
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
    Write-Host "Deleted scratch server: $Root"
    return
}
if (!$Inert -and !$AcceptEula) {
    Write-Host 'Read the Minecraft EULA: https://www.minecraft.net/eula'
    Write-Host 'Run again with -AcceptEula only if you agree. No installation or acceptance performed.'
    return
}
if (!$Inert) {
    if (!$Java) {
        if ($env:JAVA_HOME -and (Test-Path -LiteralPath (Join-Path $env:JAVA_HOME 'bin\java.exe'))) { $Java = Join-Path $env:JAVA_HOME 'bin\java.exe' }
        else {
            $detected = Get-Command java.exe -ErrorAction SilentlyContinue
            if ($detected) { $Java = $detected.Source }
        }
    }
    if (!$Java -or !(Test-Path -LiteralPath $Java -PathType Leaf)) { throw 'Java is missing. Install a JDK suitable for the selected Minecraft version, set JAVA_HOME, or pass -Java <java.exe>.' }
    $Java = [IO.Path]::GetFullPath($Java)
}
if (Test-Path -LiteralPath $Root) {
    if (!(Test-Path -LiteralPath $marker)) { throw 'Root already exists without a dev-server marker. Choose an empty dedicated -Root.' }
} else {
    New-Item -ItemType Directory -Path $Root | Out-Null
    Write-Utf8 $marker "vesta-dev-server-v1`n"
}
if (Test-Path -LiteralPath (Join-Path $Root 'agent.pid')) {
    $runningId = [int][IO.File]::ReadAllText((Join-Path $Root 'agent.pid'))
    if (Get-Process -Id $runningId -ErrorAction SilentlyContinue) { throw "Dev server PID $runningId is already running. Stop it before restarting." }
}
$buildProfile = 'debug'
$buildArgs = @()
if ($Release) { $buildProfile = 'release'; $buildArgs += '--release' }
Push-Location $repo
try {
    Invoke-Program -Executable cargo -Arguments (@('build','-p','vesta-agent','--bin','vesta-agent') + $buildArgs)
    Invoke-Program -Executable cargo -Arguments (@('build','-p','vesta-setup','--example','dev-test-support') + $buildArgs)
    $target = Join-Path $repo 'target'
    if ($env:CARGO_TARGET_DIR) { $target = [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) }
    # Stage the runtime outside Cargo's output directory so tests/rebuilds remain usable
    # while Windows holds the running executable open.
    $builtAgent = Join-Path $target "$buildProfile\vesta-agent.exe"
    $runtimeBin = Join-Path $Root 'bin'
    New-Item -ItemType Directory -Path $runtimeBin -Force | Out-Null
    $agent = Join-Path $runtimeBin 'vesta-agent.exe'
    Copy-Item -LiteralPath $builtAgent -Destination $agent -Force
    $helper = Join-Path $target "$buildProfile\examples\dev-test-support.exe"
    $installRoot = Join-Path $Root 'servers'
    New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
    $private = Join-Path $Root 'private'
    Invoke-Program -Executable $helper -Arguments @('tls',$private)
    $record = Join-Path $installRoot "names\$name.json"
    $modeFile = Join-Path $Root 'mode'
    $mode = if ($Inert) { 'inert' } else { "${Loader}:$Version" }
    if ((Test-Path -LiteralPath $modeFile) -and [IO.File]::ReadAllText($modeFile).Trim() -ne $mode) { throw 'Requested mode/version differs from this scratch server. Use -Reset first or a different -Root.' }
    if (!(Test-Path -LiteralPath $record)) {
        if ($Inert) {
            Write-Host 'INERT MODE: using the integration-test child; no Minecraft software, networking or EULA acceptance.'
            $messages = Invoke-Program -Executable cargo -Arguments (@('test','-p','vesta-transport','--test','https','--no-run','--message-format=json') + $buildArgs)
            $testExe = $null
            foreach ($line in $messages) {
                if ($line.StartsWith('{')) {
                    $artifact = ConvertFrom-Json $line
                    if ($artifact.reason -eq 'compiler-artifact' -and $artifact.target.name -eq 'https' -and $artifact.executable) { $testExe = $artifact.executable }
                }
            }
            if (!$testExe) { throw 'Cannot find integration-test child executable.' }
            Invoke-Program -Executable $helper -Arguments @('inert',$installRoot,$testExe)
        } else {
            $resolvedVersion = $Version
            if ($Version -eq 'latest') {
                [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
                $resolvedVersion = (Invoke-RestMethod 'https://piston-meta.mojang.com/mc/game/version_manifest_v2.json').latest.release
            }
            Write-Host "Installing Minecraft $Loader $resolvedVersion with $Java"
            Write-Host 'Installer acknowledgments: first-run effects and official legacy SHA-1 integrity metadata.'
            $arguments = @('create',$name,'--install-root',$installRoot,'--minecraft-version',$resolvedVersion,'--java',$Java,'--loader',$Loader,'--bind','127.0.0.1','--port',"$GamePort",'--approve-install','--ack-warnings','--ack-legacy-checksum')
            if ($LoaderVersion) { $arguments += @('--loader-version',$LoaderVersion) }
            Run-Agent -Arguments $arguments
        }
        Write-Utf8 $modeFile "$mode`n"
    }
    $instance = (ConvertFrom-Json ([IO.File]::ReadAllText($record))).instance
    if ($instance -notmatch '^srv_[0-9a-f]{32}$') { throw 'Invalid scratch instance record.' }
    $directory = Join-Path $installRoot "instances\$instance"
    $config = Join-Path $directory 'host.toml'
    if (!$Inert) { Write-Utf8 (Join-Path $directory 'game\eula.txt') "# Accepted explicitly with -AcceptEula. https://www.minecraft.net/eula`neula=true`n" }
    $document = [IO.File]::ReadAllText($config)
    $listener = "[listener]`nmode = 'tls'`nbind = '127.0.0.1'`nport = $Port`npublic-endpoint = 'https://127.0.0.1:$Port/'`ncertificate = $(Toml-String (Join-Path $private 'certificate.pem'))`nprivate-key = $(Toml-String (Join-Path $private 'key.pem'))`nmax-connections = 32`nmax-request-bytes = 1048576`nrequest-timeout-seconds = 30`npeer-requests-per-minute = 6000`npeer-connections-per-minute = 6000`nsession-requests-per-minute = 6000`n"
    $document = [regex]::Replace($document,'(?ms)^\[listener\]\s*.*?(?=^\[|\z)',$listener)
    Write-Utf8 $config $document
    Run-Agent -Arguments @('check-config',$config)
    $stdout = Join-Path $Root 'agent.stdout.log'
    $stderr = Join-Path $Root 'agent.stderr.log'
    # Start-Process uses Windows command-line parsing; reject embedded quotes/newlines.
    if ($config -match '["\r\n]') { throw 'Unsupported quote/newline in scratch path.' }
    $process = Start-Process -FilePath $agent -ArgumentList @('serve',('"' + $config + '"')) -WorkingDirectory $repo -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
    Write-Utf8 (Join-Path $Root 'agent.pid') "$($process.Id)"
    $completed = $false
    try {
        $ready = $false
        for ($attempt = 0; $attempt -lt 60; $attempt++) {
            $process.Refresh()
            if ($process.HasExited) { throw "Agent exited. Read $stderr" }
            # CLI readiness proves this instance owns its local control socket.
            if (Agent-Ready) { $ready = $true; break }
            Start-Sleep -Milliseconds 500
        }
        if (!$ready) { throw "Agent readiness timed out. Read $stderr" }
        Write-Host "`n=== LAUNCHER PAIRING: https://127.0.0.1:$Port/ ===" -ForegroundColor Cyan
        Run-Agent -Arguments @('pair',$name,'--install-root',$installRoot,'--role','owner')
        Invoke-Program -Executable $helper -Arguments @('tls',$private)
        Run-Agent -Arguments @('start',$name,'--install-root',$installRoot)
        $completed = $true
        Write-Host "Agent PID: $($process.Id); scratch: $Root"
        Write-Host "Logs: $stdout ; $stderr"
        Write-Host "Stop game: & '$agent' stop $name --install-root '$installRoot'"
        Write-Host "Then stop agent: Stop-Process -Id $($process.Id)"
        Write-Host "Delete after stopping: & '$PSCommandPath' -Root '$Root' -Reset [-Force]"
        if ($Background) { Write-Host 'Background agent is running. Pair using the link above.'; return }
        Write-Host 'Foreground session: Ctrl+C stops this disposable server.'
        while (!$process.HasExited) {
            Start-Sleep -Seconds 1
            $process.Refresh()
        }
    } finally {
        if (!$Background -or !$completed) {
            $ErrorActionPreference = 'Continue'
            & $agent stop $name --install-root $installRoot
            $process.Refresh()
            if (!$process.HasExited) { Stop-Process -Id $process.Id }
        }
    }
} finally { Pop-Location }
