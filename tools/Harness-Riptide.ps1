[CmdletBinding()]
param([Parameter(Mandatory)][string]$Script,[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A scripted PC check of the Dead Island Riptide producer, no headset and no player: monaka_play harness attaches
# monaka_riptide.dll to the running game with the play settings and runs the channel harness
# (crates\channel\examples\harness.rs) with -Script in place of the viewer: a timed head pose, controller poses and
# gamepad, saving the frames the script asks for (PNG in the run folder's frames). Start the script with
# `0 focus DeadIslandRiptideGame.exe`.
# -Experiment adds or overrides producer options (key=value: riptide\src\manifest.rs and config.rs).
# Build first: cargo build --release -p monaka -p monaka_riptide; cargo build --release -p monaka_channel --example harness
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
Invoke-MonakaPlay harness riptide --script $Script --eye 2644x2644 @Experiment
