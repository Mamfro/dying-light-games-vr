[CmdletBinding()]
param([ValidateRange(2,600)][int]$Seconds=8,[ValidateRange(0,32)][int]$Save=0,[ValidateRange(0,60)][int]$DelaySeconds=10,
    [ValidateRange(-180,180)][double]$SyntheticYaw=[double]::NaN,[ValidateRange(-180,180)][double]$HandYaw=[double]::NaN,[switch]$HandSweep,[switch]$HandSwing,[switch]$PadWalk,[ValidateSet('','none','a','b','x','y','start','back')][string]$PadHold='',[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A PC check of the DL1 producer, no headset: monaka_play capture attaches monaka_dl1.dll to the running game for a
# few seconds and reads its channel with the channel probe, which also plays the headset (it writes a head pose with
# a lopsided field of view per eye). Prints the pair rate, the pose records and the producer log; -Save keeps that
# many pairs as PNG. Head aim; -HandYaw adds a fake right controller turned that many degrees left, with a pointing
# aim (-Experiment aim=hand_rig puts the arms on it); -HandSweep pitches it 60 degrees down and up every 8 seconds;
# -HandSwing swings it like a club every 2 seconds. -PadWalk: the VR controllers' gamepad with the left stick held
# forward (the character should walk); -PadHold start (or a, b, x, y, back) holds that button on it.
# The game pauses without focus: after starting this, click into the game within -DelaySeconds.
# Build first: cargo build --release -p monaka -p monaka_dl1; cargo build --release -p monaka_channel --example probe
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
$invariant=[Globalization.CultureInfo]::InvariantCulture
$arguments=@('capture','dl1','--seconds',$Seconds,'--delay',$DelaySeconds,'aim=head')
if ($Save) { $arguments+=@('--save',$Save) }
if (-not [double]::IsNaN($SyntheticYaw)) { $arguments+="synthetic_yaw=$($SyntheticYaw.ToString($invariant))" }
if (-not [double]::IsNaN($HandYaw)) { $arguments+=@('aim=controller','--hand',$HandYaw.ToString($invariant)); if ($HandSweep) { $arguments+='--hand-sweep' }; if ($HandSwing) { $arguments+='--hand-swing' } }
if ($PadWalk) { $arguments+='--pad-walk' }
if ($PadHold) { $arguments+=@('--pad-hold',$PadHold) }
Invoke-MonakaPlay @arguments @Experiment
