param([Parameter(Mandatory=$true)][string]$LauncherRoot)
$ErrorActionPreference = 'Stop'
$canonical = Join-Path $LauncherRoot 'crates/modpack-engine'
$vendored = Join-Path $PSScriptRoot '../crates/modpack-engine'
foreach ($name in @('Cargo.toml','src/lib.rs','src/classifier.rs','src/merger.rs','src/world.rs')) {
 if ((Get-FileHash -LiteralPath (Join-Path $canonical $name)).Hash -ne (Get-FileHash -LiteralPath (Join-Path $vendored $name)).Hash) { throw "Shared modpack engine differs: $name" }
}
