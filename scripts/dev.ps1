#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Position=0)][ValidateSet('start','stop','status','recover')][string]$Action = 'status',
    [string]$Root = (Join-Path ([IO.Path]::GetTempPath()) 'vesta-dev-server')
)
$ErrorActionPreference = 'Stop'
$Root = [IO.Path]::GetFullPath($Root).TrimEnd('\','/')
if ([IO.File]::ReadAllText((Join-Path $Root '.vesta-dev-server')).Trim() -ne 'vesta-dev-server-v1') { throw 'Not a marked disposable dev server.' }
$registry = Join-Path $Root 'servers'
$id = (Get-Content -Raw -LiteralPath (Join-Path $registry 'names\dev-server.json') | ConvertFrom-Json).instance
if ($id -notmatch '^srv_[0-9a-f]{32}$') { throw 'Invalid instance record.' }
$directory = Join-Path $registry "instances\$id"
$config = Join-Path $directory 'host.toml'
$agent = Join-Path $Root 'bin\vesta-agent.exe'
function Run-Agent([string[]]$Arguments) {
    & $agent @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Agent command failed ($LASTEXITCODE)." }
}
if ($Action -eq 'status') { Run-Agent @('manage','dev-server','status','--install-root',$registry); return }
if ($Action -eq 'stop') {
    $ownedProcess = $null
    $pidFile = Join-Path $Root 'agent.pid'
    if (Test-Path -LiteralPath $pidFile) {
        $candidate = Get-Process -Id ([int][IO.File]::ReadAllText($pidFile)) -ErrorAction SilentlyContinue
        if ($candidate -and $candidate.Path -eq $agent) { $ownedProcess = $candidate }
    }
    Run-Agent @('shutdown',$config)
    if ($ownedProcess -and !$ownedProcess.WaitForExit(15000)) { throw 'Graceful shutdown is still in progress; check status/logs before restarting.' }
    return
}
# Only this marked, local scratch server is eligible. Hold the same external
# registry + instance locks as serve, and refuse links and ANY possible JVM or
# runtime owner on the host. Failure to enumerate processes fails closed.
$locks = @()
try {
    foreach ($path in @((Join-Path $registry 'registry.lock'),(Join-Path $registry "instances\$id.operator-lock"))) {
        $locks += [IO.File]::Open($path,'OpenOrCreate','ReadWrite','None')
    }
    $ancestor = $directory
    while ($ancestor) {
        if ((Get-Item -Force -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked scratch path refused.' }
        $ancestor = Split-Path -Parent $ancestor
    }
    $pending = Join-Path $directory 'operator-session-pending'
    if (Test-Path -LiteralPath $pending) {
        # Only a literal, plain Java executable is recoverable by name absence.
        # Inert children, shell wrappers and arbitrary native launchers need manual
        # recovery; their names cannot be inferred from a missing JVM.
        Run-Agent @('check-config',$config)
        $launchPath = Join-Path $directory 'launch.toml'
        $definitions = [regex]::Matches([IO.File]::ReadAllText($config),'(?m)^launch-definition\s*=\s*["'']([^"''\r\n]+)["'']\s*$')
        if ($definitions.Count -ne 1 -or [IO.Path]::GetFullPath($definitions[0].Groups[1].Value) -ne $launchPath) { throw 'Recovery requires the managed instance launch.toml.' }
        $launchTables = [regex]::Matches([IO.File]::ReadAllText($launchPath),'(?ms)^\[launch\]\s*(.*?)(?=^\[|\z)')
        if ($launchTables.Count -ne 1) { throw 'Ambiguous launch definition; recovery refused.' }
        $programs = [regex]::Matches($launchTables[0].Groups[1].Value,'(?m)^(?:java-command|executable)\s*=\s*["'']([^"''\r\n]+)["'']\s*$')
        if ($programs.Count -ne 1) { throw 'Recovery requires a literal Java launch.' }
        $program = $programs[0].Groups[1].Value
        if (![IO.Path]::IsPathRooted($program) -or [IO.Path]::GetFileName($program) -notmatch '^java(\.exe)?$') { throw 'Non-Java launch: missing JVMs cannot prove this child stopped.' }
        $programAncestor = [IO.Path]::GetFullPath($program)
        while ($programAncestor) {
            if ((Get-Item -LiteralPath $programAncestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked Java launch requires manual recovery.' }
            $programAncestor = Split-Path -Parent $programAncestor
        }
        $processes = @(Get-CimInstance Win32_Process -ErrorAction Stop)
        if (!$processes.Count) { throw 'Cannot prove process absence.' }
        $possibleOwners = @($processes | Where-Object { $_.Name -match '^(javaw?|vesta-agent|vesta-supervisor|vesta-setup|vesta)(\.exe)?$' })
        if ($possibleOwners.Count) { throw 'Recovery refused: Java or a Vesta runtime owner is running. Stop it cleanly first.' }
        if ((Get-Item -LiteralPath $pending -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked marker refused.' }
        if ([IO.File]::ReadAllText($pending) -cne "vesta-owned-agent-session-v1: shutdown-not-proven`n") { throw 'Unknown shutdown marker; manual investigation required.' }
        $history = Join-Path $directory 'operator-sessions'
        if ((Get-Item -LiteralPath $history -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked history refused.' }
        $archive = Join-Path $history ("dev-recovered-" + [guid]::NewGuid().ToString())
        Move-Item -LiteralPath $pending -Destination $archive
        Write-Host 'Archived unclean dev session after proving all JVM/runtime owners absent.'
    }
} finally { foreach ($handle in $locks) { $handle.Dispose() } }
if ($Action -eq 'recover') { return }
# Rebuild and stage outside target: a running Windows agent must not lock Cargo.
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    & cargo build -p vesta-agent --bin vesta-agent
    if ($LASTEXITCODE -ne 0) { throw 'Agent build failed.' }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    Copy-Item -LiteralPath (Join-Path $target 'debug\vesta-agent.exe') -Destination $agent -Force
    # A dev restart must leave Minecraft stopped, regardless of startup policy.
    $document = [IO.File]::ReadAllText($config)
    if ($document -match '(?m)^\s*(start-on-agent-start\s*=\s*true|on-agent-start\s*=\s*["''](?:start|sleep)["''])') {
        throw 'Set [process] on-agent-start = "stay_stopped" before a dev restart.'
    }
    $previousStartupOverride = $env:VESTA_AGENT_STAY_STOPPED
    try {
        $env:VESTA_AGENT_STAY_STOPPED = '1'
    $process = Start-Process -FilePath $agent -ArgumentList @('serve',('"' + $config + '"')) -WindowStyle Hidden -RedirectStandardOutput (Join-Path $Root 'agent.stdout.log') -RedirectStandardError (Join-Path $Root 'agent.stderr.log') -PassThru
    } finally { $env:VESTA_AGENT_STAY_STOPPED = $previousStartupOverride }

    [IO.File]::WriteAllText((Join-Path $Root 'agent.pid'),$process.Id.ToString())
    for ($attempt=0; $attempt -lt 60; $attempt++) {
        $process.Refresh()
        if ($process.HasExited) { throw "Agent exited; read $Root\agent.stderr.log" }
        $ErrorActionPreference = 'Continue'
        & $agent access show dev-server --install-root $registry *> $null
        $ErrorActionPreference = 'Stop'
        if ($LASTEXITCODE -eq 0) { Write-Host "Dev agent ready (PID $($process.Id)); Minecraft remains stopped."; return }
        Start-Sleep -Milliseconds 500
    }
    throw 'Agent readiness timed out.'
} finally { Pop-Location }
