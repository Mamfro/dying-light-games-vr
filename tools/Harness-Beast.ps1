[CmdletBinding()]
param([Parameter(Mandatory)][string]$Script,[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A scripted PC check of the Dying Light: The Beast producer, no headset and no player: monaka_play harness attaches
# monaka_beast.dll to the running game (DirectX 12) with stereo and motion controls and runs the channel harness
# (crates\channel\examples\harness.rs) with -Script in place of the viewer: a timed head pose, controller poses, fingers
# and gamepad, saving the frames the script asks for (PNG in the run folder's frames). The game pauses without focus:
# start the script with `0 focus DyingLightGame_TheBeast_x64_rwdi.exe`.
# -Experiment adds or overrides producer options (key=value: beast\src\manifest.rs and config.rs), for example
# probe_hands=1 or mode=framegen.
# Build first: cargo build --release -p monaka -p monaka_beast; cargo build --release -p monaka_channel --example harness
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
Invoke-MonakaPlay harness beast --script $Script aim=hand_rig @Experiment
