[CmdletBinding()]
param([Parameter(Mandatory)][string]$Script,[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A scripted PC check of the DL1 producer, no headset and no player: monaka_play harness attaches monaka_dl1.dll to the
# running game with the play settings (motion controls, the headset eye size) and runs the channel harness
# (crates\channel\examples\harness.rs) with -Script in place of the viewer: a timed head pose, controller poses and
# gamepad, saving the frames the script asks for (PNG in the run folder's frames). DL1 pauses without focus: start the
# script with `0 focus DyingLightGame.exe`.
# -Experiment adds or overrides producer options (key=value: dl1\src\manifest.rs and config.rs).
# Build first: cargo build --release -p monaka -p monaka_dl1; cargo build --release -p monaka_channel --example harness
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
Invoke-MonakaPlay harness dl1 --script $Script --eye 2644x2644 @Experiment
