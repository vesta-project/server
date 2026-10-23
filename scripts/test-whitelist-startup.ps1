param(
    [Parameter(Mandatory=$true)][string]$ServerJar,
    [Parameter(Mandatory=$true)][string]$Libraries,
    [Parameter(Mandatory=$true)][string]$AcceptedEula,
    [string]$Java = 'java',
    [string]$OutputDirectory = (Join-Path ([IO.Path]::GetTempPath()) ('vesta-whitelist-probe-' + [guid]::NewGuid().ToString('N')))
)
$ErrorActionPreference='Stop'
if ((Get-Content -LiteralPath $AcceptedEula -Raw) -notmatch '(?m)^eula=true\s*$') {throw 'An already accepted EULA file is required.'}
if (Test-Path -LiteralPath $OutputDirectory) {throw 'Output directory must be new.'}
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
Copy-Item -LiteralPath $AcceptedEula -Destination (Join-Path $OutputDirectory 'eula.txt')
$encoding=[Text.UTF8Encoding]::new($false)
$whitelistPath=Join-Path $OutputDirectory 'whitelist.json'
function Write-ProbeWhitelist([string[]]$Names) {
    $entries=@(foreach ($name in $Names) {
        $md5=[Security.Cryptography.MD5]::Create()
        try {$bytes=$md5.ComputeHash([Text.Encoding]::UTF8.GetBytes('OfflinePlayer:'+$name))} finally {$md5.Dispose()}
        $bytes[6]=($bytes[6] -band 15) -bor 48; $bytes[8]=($bytes[8] -band 63) -bor 128
        $hex=([BitConverter]::ToString($bytes)).Replace('-','').ToLowerInvariant()
        @{name=$name;uuid=($hex.Substring(0,8)+'-'+$hex.Substring(8,4)+'-'+$hex.Substring(12,4)+'-'+$hex.Substring(16,4)+'-'+$hex.Substring(20))}
    })
    [IO.File]::WriteAllText($whitelistPath,(ConvertTo-Json -InputObject $entries),$encoding)
}
# Reserve an ephemeral loopback address for configuration; the child proves its bind.
$listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0); $listener.Start()
$port=$listener.LocalEndpoint.Port; $listener.Stop()
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'server.properties'),"online-mode=false`nwhite-list=true`nenforce-whitelist=true`nserver-ip=127.0.0.1`nserver-port=$port`nview-distance=2`nsimulation-distance=2`nlevel-type=minecraft:flat`ngenerate-structures=false`nmax-players=1`npause-when-empty-seconds=0`n",$encoding)
Write-ProbeWhitelist @('vestabefore','vestaremove')
$classpath=@((Resolve-Path -LiteralPath $ServerJar).Path)+@(Get-ChildItem -LiteralPath $Libraries -Filter '*.jar' -Recurse -File | ForEach-Object FullName)
$start=[Diagnostics.ProcessStartInfo]::new()
$start.FileName=$Java; $start.WorkingDirectory=$OutputDirectory
$start.Arguments='-Xms128M -Xmx512M -XX:ActiveProcessorCount=2 -cp "'+($classpath -join [IO.Path]::PathSeparator)+'" net.minecraft.server.Main --nogui'
$start.UseShellExecute=$false; $start.CreateNoWindow=$true
$start.RedirectStandardInput=$true; $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
$process=[Diagnostics.Process]::new(); $process.StartInfo=$start
$log=[IO.StreamWriter]::new((Join-Path $OutputDirectory 'console.log'),$false,$encoding)
$started=$false
$stage='startup'; $injected=$false; $results=[ordered]@{directory=$OutputDirectory;port=$port;startup_injection=$false;checks=@()}
try {
    if (!$process.Start()) {throw 'Fixture did not start.'}
    $started=$true
    $stderr=$process.StandardError.ReadToEndAsync()
    $next=$process.StandardOutput.ReadLineAsync(); $timer=[Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt 150) {
        if (!$next.Wait(100)) {continue}
        $line=$next.Result
        if ($null -eq $line) {break}
        $log.WriteLine($line); $log.Flush()
        if (!$injected -and $line -match 'Preparing level') {
            Write-ProbeWhitelist @('vestabefore','vestaduring')
            $injected=$true; $results.startup_injection=$true
            Write-Output 'Injected add and removal during Preparing level, before Done.'
        }
        if ($stage -eq 'startup' -and $line -match 'Done \(') {
            if (!$injected) {throw 'Startup injection marker was not observed; no startup conclusion is valid.'}
            $process.StandardInput.WriteLine('whitelist list'); $process.StandardInput.Flush(); $stage='initial-list'
        } elseif ($stage -eq 'initial-list' -and $line -match 'whitelisted player') {
            if ($line -notmatch 'vestabefore' -or $line -notmatch 'vestaremove' -or $line -match 'vestaduring') {throw "Unexpected startup memory observation: $line"}
            $results.checks+=@{check='startup_file_edits_not_in_memory';line=$line}
            $process.StandardInput.WriteLine('whitelist add vestaafter');$process.StandardInput.Flush();$stage='after-add'
        } elseif ($stage -eq 'after-add' -and $line -match 'Added vestaafter') {
            $names=@((Get-Content -LiteralPath $whitelistPath -Raw | ConvertFrom-Json).name)
            if ($names -contains 'vestaduring' -or $names -notcontains 'vestaremove' -or $names -notcontains 'vestaafter') {throw 'Command did not exhibit the expected persisted overwrite.'}
            $results.checks+=@{check='command_overwrites_startup_file_edits';names=$names}
            $process.StandardInput.WriteLine('whitelist add vestaduring');$process.StandardInput.WriteLine('whitelist remove vestaremove');$process.StandardInput.WriteLine('whitelist list');$process.StandardInput.Flush();$stage='reconciled'
        } elseif ($stage -eq 'reconciled' -and $line -match 'whitelisted player') {
            if ($line -notmatch 'vestabefore' -or $line -notmatch 'vestaafter' -or $line -notmatch 'vestaduring' -or $line -match 'vestaremove') {throw "Command reconciliation failed: $line"}
            $results.checks+=@{check='post_ready_commands_apply_desired_changes';line=$line}
            Write-ProbeWhitelist @('vestabefore')
            $process.StandardInput.WriteLine('whitelist list');$process.StandardInput.Flush();$stage='before-reload'
        } elseif ($stage -eq 'before-reload' -and $line -match 'whitelisted player') {
            if ($line -notmatch 'vestaduring') {throw 'Expected the file-only edit to leave live memory unchanged.'}
            $process.StandardInput.WriteLine('whitelist reload');$process.StandardInput.WriteLine('whitelist list');$process.StandardInput.Flush();$stage='reload'
        } elseif ($stage -eq 'reload' -and $line -match 'whitelisted player') {
            if ($line -notmatch 'vestabefore' -or $line -match 'vestaduring|vestaafter|vestaremove') {throw "Reload did not load file: $line"}
            $results.checks+=@{check='explicit_reload_applies_file';line=$line}
            $process.StandardInput.WriteLine('whitelist add VestaCaseProbe');$process.StandardInput.Flush();$stage='case-profile'
        } elseif ($stage -eq 'case-profile' -and $line -match 'Added VestaCaseProbe') {
            $caseEntries=Get-Content -LiteralPath $whitelistPath -Raw | ConvertFrom-Json
            $entry=$caseEntries | Where-Object { $_.name -ieq 'VestaCaseProbe' }
            if (!$entry -or @($entry).Count -ne 1) {throw 'Expected one case-probe entry.'}
            $results.checks+=@{check='offline_command_profile_case_observation';requested='VestaCaseProbe';stored=$entry}
            $process.StandardInput.WriteLine('stop');$process.StandardInput.Flush();$stage='complete'
        }
        $next=$process.StandardOutput.ReadLineAsync()
    }
    if ($stage -ne 'complete') {throw "Probe incomplete at $stage. See console.log."}
    if (!$process.WaitForExit(20000)) {throw 'Fixture failed to stop gracefully.'}
    $results.exit_code=$process.ExitCode
    if ($process.ExitCode -ne 0) {throw 'Fixture exited unsuccessfully.'}
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'result.json'),($results | ConvertTo-Json -Depth 8),$encoding)
    Write-Output ($results | ConvertTo-Json -Depth 8)
} finally {
    if ($started -and !$process.HasExited) {
        try {$process.StandardInput.WriteLine('stop');$process.StandardInput.Flush()} catch {}
        if (!$process.WaitForExit(15000)) {$process.Kill();$process.WaitForExit()}
    }
    if ($stderr) {[IO.File]::WriteAllText((Join-Path $OutputDirectory 'stderr.log'),$stderr.GetAwaiter().GetResult(),$encoding)}
    $log.Dispose();$process.Dispose()
}
