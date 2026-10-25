# Start outside `cargo run`: Cargo's Windows job can prevent agent breakaway.
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$vestaDevExecutable = $null
Push-Location $repo
try {
    & cargo build --locked -p vesta-dev --message-format=json-render-diagnostics |
        ForEach-Object {
            $message = $_ | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq 'vesta-dev' -and $message.executable) {
                $vestaDevExecutable = $message.executable
            }
        }
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the development shell.' }
    if (!$vestaDevExecutable) { throw 'Cargo did not report the development executable.' }
    & $vestaDevExecutable @args
    if ($LASTEXITCODE -ne 0) { throw 'Development session failed.' }
} finally { Pop-Location }
