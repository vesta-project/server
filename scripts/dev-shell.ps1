# Start outside `cargo run`: Cargo's Windows job can prevent agent breakaway.
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$vestaDevExecutable = $null
$vestaShellDirectory = $null
Push-Location $repo
try {
    # Match the helper's package set so backend changes compile once with one
    # unified feature set, rather than once for the helper and again for agents.
    $cargoArguments = @('build','--locked','-p','vesta-dev','-p','vesta-agent','-p','vesta-setup','--bins','--message-format=json-render-diagnostics')
    if ($args -contains '--release') { $cargoArguments += '--release' }
    & cargo @cargoArguments |
        ForEach-Object {
            $message = $_ | ConvertFrom-Json
            if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq 'vesta-dev' -and $message.executable) {
                $vestaDevExecutable = $message.executable
            }
        }
    if ($LASTEXITCODE -ne 0) { throw 'Could not build the development tool.' }
    if (!$vestaDevExecutable) { throw 'Cargo did not report the development executable.' }
    # Do not hold target/debug/vesta-dev.exe open for the session lifetime.
    # Another terminal must still be able to rebuild the helper on Windows.
    $vestaShellDirectory = Join-Path ([IO.Path]::GetTempPath()) ('vesta-dev-shell-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($vestaShellDirectory) | Out-Null
    $vestaShellExecutable = Join-Path $vestaShellDirectory 'vesta-dev.exe'
    Copy-Item -LiteralPath $vestaDevExecutable -Destination $vestaShellExecutable
    & $vestaShellExecutable @args
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} catch {
    [Console]::Error.WriteLine('vesta-dev: ' + $_.Exception.Message)
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    exit 1
} finally {
    Pop-Location
    if ($vestaShellDirectory) {
        # Exact owned file, then empty directory only. Never recursively delete.
        Remove-Item -LiteralPath (Join-Path $vestaShellDirectory 'vesta-dev.exe') -ErrorAction SilentlyContinue
        try { [IO.Directory]::Delete($vestaShellDirectory, $false) } catch { }
    }
}
