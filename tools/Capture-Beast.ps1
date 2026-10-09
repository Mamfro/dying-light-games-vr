[CmdletBinding()]
param([ValidateRange(2,600)][int]$Seconds=8,[ValidateRange(0,32)][int]$Save=4,[ValidateRange(0,60)][int]$DelaySeconds=0,
    [ValidateRange(0,8)][int]$Latency=2,[ValidateRange(-30,30)][double]$EyeTest=0,[ValidateRange(-30,30)][double]$LatencyTest=0,
    [ValidateRange(-180,180)][double]$SyntheticYaw=0,[ValidateRange(-89,89)][double]$SyntheticPitch=0,[ValidatePattern('^\d+x\d+$')][string]$EyeSize,[ValidateRange(-180,180)][double]$HandYaw=[double]::NaN,[switch]$HandSwing,
    [ValidatePattern('^[a-z_0-9]+=[\w.:,\-]+$')][string[]]$Experiment=@())
# A PC check of the Dying Light: The Beast stereo producer, no headset: monaka_play capture attaches monaka_beast.dll
# to the running game (DirectX 12) with a synthetic head for a few seconds and reads its channel with the channel
# probe. Prints the pair rate and the producer log; -Save keeps that many pairs as PNG in the run folder. -EyeTest
# turns the left eye that many degrees left and the right eye as far right, to check which frame is which eye
# (-Latency). -LatencyTest turns every other pair's head that many degrees left; the log names each pair's recorded
# yaw, to compare with the saved images (the pose records' timing). -HandYaw adds a fake right controller turned that
# many degrees left (-HandSwing swings it like a club every 2 seconds); with -Experiment aim=hand_rig the arms follow
# it. Click into the game first: it pauses without focus, and the monitor shows the eyes alternating while attached.
# Build first: cargo build --release -p monaka -p monaka_beast; cargo build --release -p monaka_channel --example probe
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
$invariant=[Globalization.CultureInfo]::InvariantCulture
$arguments=@('capture','beast','--seconds',$Seconds,'--delay',$DelaySeconds,"latency=$Latency")
if ($Save) { $arguments+=@('--save',$Save) }
# A fake controller sits where the probe's own head (1.7 m up) would hold it: the probe's head then, not a synthetic one.
if ([double]::IsNaN($HandYaw)) { $arguments+=@(('synthetic_yaw=' + $SyntheticYaw.ToString($invariant)),('synthetic_pitch=' + $SyntheticPitch.ToString($invariant))) }
else { $arguments+=@('--hand',$HandYaw.ToString($invariant)); if ($HandSwing) { $arguments+='--hand-swing' } }
if ($EyeTest) { $arguments+=('eye_test=' + $EyeTest.ToString($invariant)) }
if ($LatencyTest) { $arguments+=('latency_test=' + $LatencyTest.ToString($invariant)) }
if ($EyeSize) { $arguments+=@('--eye',$EyeSize) }
Invoke-MonakaPlay @arguments @Experiment
