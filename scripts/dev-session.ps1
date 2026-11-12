# Loaded only in the child shell launched by cargo dev. Compatible with PowerShell 5.1.
$ErrorActionPreference = 'Stop'
function global:Update-VestaDevPath {
    $next = [IO.File]::ReadAllText((Join-Path $env:VESTA_DEV_SESSION 'current-bin'))
    foreach ($name in @('vesta','vesta-agent','vesta-supervisor','vesta-setup')) {
        if (!(Test-Path -LiteralPath (Join-Path $next ($name + '.exe')) -PathType Leaf)) { throw 'Incomplete development build' }
    }
    $parts = @($env:PATH -split ';' | Where-Object { $_ -and $_ -ne $env:VESTA_DEV_BIN })
    $env:PATH = (@($next) + $parts) -join ';'
    $env:VESTA_DEV_BIN = $next
}
function global:vesta-rebuild {
    $buildArgs = @('build','--session',$env:VESTA_DEV_SESSION)
    if ($env:VESTA_DEV_RELEASE -eq '1') { $buildArgs += '--release' }
    $buildArgs += $args
    $savedPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $env:VESTA_DEV_TOOL @buildArgs
        $buildExit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $savedPreference
    }
    Update-VestaDevPath
    if ($buildExit -ne 0) { return }
    Write-Host 'Commands now use the rebuilt binaries. Only an explicitly selected --restart server is restarted.'
}
Update-VestaDevPath
Write-Host "Vesta dev session | registry: $env:VESTA_SERVER_ROOT"
Write-Host 'vesta list | vesta manage <id> | vesta-setup preview | vesta-rebuild [--restart <id>] | exit'
