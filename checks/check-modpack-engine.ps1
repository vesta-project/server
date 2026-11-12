param([Parameter(Mandatory=$true)][string]$LauncherRoot)
$ErrorActionPreference = 'Stop'
$canonical = Join-Path $LauncherRoot 'crates/modpack-engine'
$vendored = Join-Path $PSScriptRoot '../crates/modpack-engine'
foreach ($name in @('Cargo.toml','src/lib.rs','src/classifier.rs','src/merger.rs','src/world.rs')) {
 # Git checkouts can differ in CRLF policy; compare normalized source text.
 if ([IO.File]::ReadAllText((Join-Path $canonical $name)).Replace("`r`n","`n") -cne [IO.File]::ReadAllText((Join-Path $vendored $name)).Replace("`r`n","`n")) { throw "Shared modpack engine differs: $name" }
}
