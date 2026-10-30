# Build and update the selected dev agent. Cargo exits before the helper runs.
$ErrorActionPreference = 'Stop'
if ($args -contains '--help' -or $args -contains '-h') {
    @'
Vesta development

  .\dev.ps1                         Build and update dev-server
  .\dev.ps1 restart [server]        Build and update a selected server
  .\dev.ps1 build                   Build without restarting
  .\dev.ps1 shell                   Open the optional tools shell

Options: --release, --server-root <registry>, --local-only, --configured-listener.
Updates preserve Minecraft's running/stopped state and return to this terminal.
'@
    return
}
$devArguments = @($args)
if (!$devArguments.Count -or $devArguments[0] -notin @('update','restart','build','shell','--help','-h')) {
    $devArguments = @('update') + $devArguments
}
& (Join-Path $PSScriptRoot 'scripts/dev-shell.ps1') @devArguments
if ($LASTEXITCODE) { exit $LASTEXITCODE }
