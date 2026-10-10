[CmdletBinding()]
param([ValidateSet('Hand','Controller','Head','Mouse')][string]$Aim='Hand',[ValidateSet('left','right')][string]$AimHand='right',[switch]$FlatHud,
    [switch]$GameResolution,[switch]$NoLaunch,[ValidateRange(0.03,0.09)][double]$EyeSeparation=0.064,[ValidateRange(0.5,20)][double]$HudDistance=2.0,[switch]$FingerBinary,
    [ValidateRange(0.2,1.0)][double]$HudScale=0.36,[ValidateRange(0,120)][int]$SettleSeconds=20,[ValidateRange(-180,180)][double]$SyntheticYaw=[double]::NaN,
    [ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@(),
    # Compatibility switches: -HeadAim, -ControllerAim, -NoHeadAim are -Aim Head, Controller, Mouse; the rest turn on
    # what is the default anyway.
    [switch]$HeadAim,[switch]$ControllerAim,[switch]$NoHeadAim,[switch]$HandRig,[switch]$HandAim,[switch]$HeadsetResolution,[switch]$WorldHud)
# Play Dying Light 1 in VR: .\tools\Play-DL1VR.ps1 with nothing else is the whole of it. It runs monaka_play, which attaches
# the DL1 producer (monaka_dl1.dll) and shows it with monaka_viewer; starts the game through Steam if it is not running
# (-NoLaunch: only attach), and restarts the run if it ends. Run folders go to Monaka's runs folder.
# Requirements: SteamVR up with the headset connected; NVIDIA HBAO+, depth of field and PCSS off in the game's video
# options (they get the wrong camera and make shadows slide); no /3dtv launch option.
#
# By default:
# - your hands and the weapon are on the controllers, the arms reaching them, and the weapon (-AimHand right, or left)
#   aims where it points; swing it to hit. With that controller off or untracked, the head aims.
# - the in-world HUD: health, the weapon, the quick item, the minimap and the quests on your hands; the rest of the HUD
#   floats in front of you.
# - the game renders at the headset's eye size while VR runs, and gets its own size back at the stop (it saves either to
#   video.scr; if the game crashes mid-run, set it back in its options).
# -Aim picks what aims the weapon (one of four, as the launcher's Aim):
#   Hand        (default) the hand and the weapon on the controller, as above.
#   Controller  the controller aims (a ring in the headset marks where); the game animates the arms.
#   Head        the head aims: the character looks, aims and attacks where you look; the game animates the arms.
#   Mouse       the mouse and stick aim and turn as on a monitor; the head only moves the view.
# To leave the rest out:
# -FlatHud       the whole HUD in front of you (the in-world HUD reads the game's UI every frame: a little performance).
# -GameResolution the game's own resolution.
# Tuning: -HudDistance is how far away the HUD appears (metres); -HudScale its size (1 = monitor layout). If the hand sits wrong on the controller, move the wrist with
# -Experiment hand_hold=x,y,z (metres in the palm's space: x right, y up, z back along the pointing finger).
# -SyntheticYaw (PC checks only) replaces the headset pose with a fixed one turned that many degrees.
# -Experiment passes extra producer options (key=value; see dl1\src\config.rs), for example probe_look=1 (log the
# player camera's look fields). The probes write to monaka_dl1.log in the run folder.
# The session itself is monaka_launch (crates\launch): this script only turns its parameters into monaka_play options,
# passing every one so its defaults stay these (monaka_play list shows them).
# Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_dl1
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
$play=Join-Path $MonakaRoot 'target\release\monaka_play.exe'
if (-not (Test-Path -LiteralPath $play)) { throw "Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_dl1 (missing $play)" }
$invariant=[Globalization.CultureInfo]::InvariantCulture
function OnOff([bool]$on) { if ($on) { 'on' } else { 'off' } }
# The compatibility switches, strongest first (-NoHeadAim: no head aim, so no hand aim or hand either).
if ($NoHeadAim) { $Aim='Mouse' } elseif ($HeadAim) { $Aim='Head' } elseif ($ControllerAim) { $Aim='Controller' }
$aimOption=@{ Hand='hand_rig'; Controller='controller'; Head='head'; Mouse='off' }[$Aim]
$extra=@($Experiment)
if (-not [double]::IsNaN($SyntheticYaw)) { $extra+="synthetic_yaw=$($SyntheticYaw.ToString($invariant))" }
& $play dl1 "aim=$aimOption" "aim_hand=$AimHand" "render_size=$(if ($GameResolution) { 'game' } else { 'headset' })" `
    "hud_scale=$($HudScale.ToString($invariant))" "hud_distance=$($HudDistance.ToString($invariant))" "finger_binary=$(OnOff $FingerBinary)" "eye_separation=$($EyeSeparation.ToString($invariant))" `
    "world_hud=$(OnOff (-not $FlatHud))" "extra=$($extra -join ' ')" "launch=$(OnOff (-not $NoLaunch))" "settle_seconds=$SettleSeconds"
exit $LASTEXITCODE
