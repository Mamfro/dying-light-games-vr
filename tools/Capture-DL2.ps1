[CmdletBinding()]
param([ValidateRange(2,600)][int]$Seconds=8,[ValidateRange(0,32)][int]$Save=0,[ValidateRange(0,60)][int]$DelaySeconds=2,
    [ValidateRange(-180,180)][double]$HandYaw=[double]::NaN,[switch]$HandSweep,[switch]$HandSwing,[switch]$HandRig,[switch]$ProbeHands,
    [switch]$FrameGen,[ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# A PC check of the DL2 producer, no headset: monaka_play capture attaches monaka_dl2.dll to the running game for a few
# seconds and reads its channel with the channel probe, which also plays the headset (a head pose with a lopsided
# field of view per eye) and, with -HandYaw, a right controller held out in front turned that many degrees left
# (-HandSweep pitches it slowly, -HandSwing swings it like a club every 2 seconds). Prints the pair rate and the
# producer log; -Save keeps that many pairs as PNG. -HandRig puts the arms on the fake controller, -ProbeHands logs
# the arms skeleton, -Experiment adds or overrides producer options (key=value: dl2\src\manifest.rs and
# config.rs, e.g. swing_attack=0). Same-frame stereo (Sharp) unless -FrameGen.
# The game pauses without focus: after starting this, click into the game within -DelaySeconds.
# Build first: cargo build --release -p monaka -p monaka_dl2; cargo build --release -p monaka_channel --example probe
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
$invariant=[Globalization.CultureInfo]::InvariantCulture
$aim=if ($HandRig) { 'hand_rig' } elseif (-not [double]::IsNaN($HandYaw)) { 'controller' } else { 'head' }
$arguments=@('capture','dl2','--seconds',$Seconds,'--delay',$DelaySeconds,"aim=$aim","mode=$(if ($FrameGen) { 'framegen' } else { 'same' })")
if ($Save) { $arguments+=@('--save',$Save) }
if ($ProbeHands) { $arguments+='probe_hands=1' }
if (-not [double]::IsNaN($HandYaw)) { $arguments+=@('--hand',$HandYaw.ToString($invariant)); if ($HandSweep) { $arguments+='--hand-sweep' }; if ($HandSwing) { $arguments+='--hand-swing' } }
Invoke-MonakaPlay @arguments @Experiment
