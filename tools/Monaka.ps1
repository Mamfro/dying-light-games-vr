# Dot-source this from the other scripts: $MonakaRoot is the Monaka checkout these scripts run
# (monaka_play, the loader, the runs folder): MONAKA_ROOT when set, else `monaka_vr` beside the
# `monaka_adapters` folder this repository sits in. Build the Dying Light DLLs into its `target` (cargo-config.example.toml) so they sit
# beside the launcher.
$MonakaRoot=if ($env:MONAKA_ROOT) { $env:MONAKA_ROOT } else { Join-Path (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))) 'monaka_vr' }
if (-not (Test-Path -LiteralPath $MonakaRoot)) { throw "No Monaka checkout at ${MonakaRoot}: clone it beside monaka_adapters as monaka_vr, or set MONAKA_ROOT." }
