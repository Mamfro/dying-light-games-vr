# Dot-source this from a Harness-*, Capture-* or Probe-*.ps1: runs Monaka's release monaka_play with the arguments
# given (its harness, capture and probe subcommands make the run folder, write the game's options from its manifest,
# attach the producer, run the stand-in for the viewer, detach and print the log). The script's own exit code is
# monaka_play's.
. (Join-Path $PSScriptRoot 'Monaka.ps1')
function Invoke-MonakaPlay {
    param([Parameter(ValueFromRemainingArguments)][string[]]$Arguments)
    $play=Join-Path $MonakaRoot 'target\release\monaka_play.exe'
    if (-not (Test-Path -LiteralPath $play)) { throw "Build Monaka first: cargo build --release -p monaka (missing $play)" }
    & $play @Arguments
    exit $LASTEXITCODE
}
