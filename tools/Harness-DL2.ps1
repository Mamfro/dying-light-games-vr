[CmdletBinding()]
param([Parameter(Mandatory)][string]$Script,[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A scripted PC check of the DL2 producer, no headset and no player: monaka_play harness attaches monaka_dl2.dll to the
# running game with the play settings (motion controls, Smooth) and runs the channel harness
# (crates\channel\examples\harness.rs) with -Script in place of the viewer: a timed head pose, controller poses, fingers
# and gamepad, saving the frames the script asks for (PNG in the run folder's frames). DL2 pauses without focus: start
# the script with `0 focus DyingLightGame_x64_rwdi.exe`.
# -Experiment adds or overrides producer options (key=value: the launcher's, dl2\src\manifest.rs, and the
# producer's own, dl2\src\config.rs), for example mode=depth, hud_scale=0.4, probe_hud=1, aim=head.
# Build first: cargo build --release -p monaka -p monaka_dl2; cargo build --release -p monaka_channel --example harness
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
Invoke-MonakaPlay harness dl2 --script $Script @Experiment
