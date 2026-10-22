param(
    [ValidateRange(5, 60)][int]$Seconds = 20,
    [string]$Agent = (Join-Path $PSScriptRoot '../target/release/vesta-agent.exe'),
    [switch]$ThreadBreakdown
)
$ErrorActionPreference = 'Stop'
$binary = (Resolve-Path -LiteralPath $Agent).Path
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) ('vesta-idle-review-' + [guid]::NewGuid().ToString('N'))
foreach ($directory in @('', 'game/config', 'bin', 'private')) {
    New-Item -ItemType Directory -Path (Join-Path $fixture $directory) -Force | Out-Null
}
Copy-Item -LiteralPath (Join-Path $repo 'crates/vesta-transport/tests/fixtures/test-only-server.pem') -Destination (Join-Path $fixture 'private/cert.pem')
Copy-Item -LiteralPath (Join-Path $repo 'crates/vesta-transport/tests/fixtures/test-only-server-key.pem') -Destination (Join-Path $fixture 'private/key.pem')
# No Java or real executable is present. Omitted process policy defaults to no
# auto-start; all paths, state and known test-only TLS material are fixture-owned.
[IO.File]::WriteAllText((Join-Path $fixture 'bin/inert-placeholder'), '')
[IO.File]::WriteAllText((Join-Path $fixture 'private/launch.toml'), @'
[minecraft]
working-directory = 'game'
[launch]
kind = 'native'
executable = '../bin/inert-placeholder'
stop = 'console-stop'
'@)
$socket = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$socket.Start()
$port = $socket.LocalEndpoint.Port
$socket.Stop()
[IO.File]::WriteAllText((Join-Path $fixture 'host.toml'), @"
schema-version = 1
[paths]
state = 'private-state'
game = 'game'
scan-roots = ['game/config']
launch-definition = 'private/launch.toml'
[pack]
name = 'Owned idle performance fixture'
version = '1.0.0'
revision = 1
minimum-revision = 1
minecraft-version = '1.21.1'
unresolved-disposition = 'embed-public'
[pack-access]
mode = 'minecraft'
[listener]
mode = 'tls'
bind = '127.0.0.1'
port = $port
certificate = 'private/cert.pem'
private-key = 'private/key.pem'
max-connections = 16
max-request-bytes = 1048576
request-timeout-seconds = 10
"@)
$owned = Start-Process -FilePath $binary -ArgumentList @('serve', 'host.toml') -WorkingDirectory $fixture -WindowStyle Hidden -RedirectStandardOutput (Join-Path $fixture 'stdout.log') -RedirectStandardError (Join-Path $fixture 'stderr.log') -PassThru
$started = $owned.StartTime
try {
    $owned.ProcessorAffinity = [IntPtr]1
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $owned.Refresh()
        if ($owned.HasExited) { throw ('Fixture failed: ' + (Get-Content -LiteralPath (Join-Path $fixture 'stderr.log') -Raw)) }
        $probe = [Net.Sockets.TcpClient]::new()
        try { $probe.Connect('127.0.0.1', $port); $ready = $true } catch { $ready = $false } finally { $probe.Dispose() }
        if (!$ready) { Start-Sleep -Milliseconds 50 }
    } until ($ready -or [DateTime]::UtcNow -gt $deadline)
    if (!$ready) { throw 'Owned fixture listener did not become ready' }
    Start-Sleep -Seconds 2
    $owned.Refresh()
    $cpu = $owned.TotalProcessorTime.TotalSeconds
    $threadCpu = @{}
    if ($ThreadBreakdown) { foreach ($thread in $owned.Threads) { $threadCpu[$thread.Id] = $thread.TotalProcessorTime.TotalSeconds } }
    $wall = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $Seconds
    $owned.Refresh()
    $wall.Stop()
    $breakdown = @()
    if ($ThreadBreakdown) {
        if (-not ('VestaIdleThreadNames' -as [type])) {
            Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class VestaIdleThreadNames {
  [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint access, bool inherit, uint id);
  [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr thread, out IntPtr text);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
  public static string Get(uint id) {
    var thread = OpenThread(0x0800, false, id);
    if (thread == IntPtr.Zero) return "unavailable";
    IntPtr text = IntPtr.Zero;
    try { return GetThreadDescription(thread, out text) == 0 ? Marshal.PtrToStringUni(text) : "unnamed"; }
    finally { if(text != IntPtr.Zero) LocalFree(text); CloseHandle(thread); }
  }
}
'@
        }
        $breakdown = @(foreach ($thread in $owned.Threads) {
            if ($threadCpu.ContainsKey($thread.Id)) {
                [pscustomobject]@{ name = [VestaIdleThreadNames]::Get($thread.Id); cpuSeconds = [Math]::Round($thread.TotalProcessorTime.TotalSeconds - $threadCpu[$thread.Id], 6) }
            }
        }) | Sort-Object -Property cpuSeconds -Descending
    }
    [pscustomobject]@{
        fixture = $fixture; binary = $binary; sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        seconds = [Math]::Round($wall.Elapsed.TotalSeconds, 3)
        cpuSeconds = [Math]::Round($owned.TotalProcessorTime.TotalSeconds - $cpu, 6)
        percentOfOneCore = [Math]::Round(100 * ($owned.TotalProcessorTime.TotalSeconds - $cpu) / $wall.Elapsed.TotalSeconds, 3)
        workingSetMiB = [Math]::Round($owned.WorkingSet64 / 1MB, 3)
        privateMiB = [Math]::Round($owned.PrivateMemorySize64 / 1MB, 3)
        threads = $owned.Threads.Count; affinity = $owned.ProcessorAffinity.ToInt64()
        minecraft = 'absent'; clients = 0; build = 'release'; platform = 'Windows'
        threadCpu = $breakdown
    } | ConvertTo-Json
} finally {
    # Only the process created above, with matching executable AND start time.
    # Its fixture has no runnable game. Preserve logs/state for review; no delete.
    $current = Get-Process -Id $owned.Id -ErrorAction SilentlyContinue
    if ($current -and $current.Path -eq $binary -and $current.StartTime -eq $started) { Stop-Process -Id $current.Id }
}
