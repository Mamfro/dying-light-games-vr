[CmdletBinding()]
param([ValidateRange(2,120)][int]$Seconds=10,[ValidateRange(0,60)][int]$DelaySeconds=0,[ValidateRange(-90,90)][double]$TurnYaw=0)
# Probe of Dying Light: The Beast's renderer camera, no headset: monaka_play probe attaches monaka_beast.dll to the
# running game for a few seconds, then stops it and prints its log (which cameras are set or rebuilt, from where, how
# often). It changes nothing in the game, except with -TurnYaw: the player camera is then turned by that many degrees
# while attached (the PC check that writing it reaches the picture). Click into the game first: it pauses without focus.
# Build first: cargo build --release -p monaka -p monaka_beast
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Check.ps1')
$arguments=@('probe','beast','--seconds',$Seconds,'--delay',$DelaySeconds,'stereo=0','probe=1')
if ($TurnYaw) { $arguments+=('turn_yaw=' + $TurnYaw.ToString([Globalization.CultureInfo]::InvariantCulture)) }
Invoke-MonakaPlay @arguments
