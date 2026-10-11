[CmdletBinding()]
param([switch]$NoLaunch,[ValidateRange(0,180)][int]$SettleSeconds=30,[switch]$HeadsetResolution,[switch]$GameResolution,[switch]$SameFrame,[switch]$FrameGen,[switch]$DepthStereo,[switch]$AlternateEye,[ValidateRange(0.1,1.0)][double]$HudScale=0.55,[switch]$LockPitch,[ValidateSet('Hand','Controller','Head','Mouse')][string]$Aim='Hand',[switch]$HeadAim,[switch]$NoHeadAim,[switch]$HandAim,[switch]$HandRig,[ValidateSet('left','right')][string]$AimHand='right',[switch]$FingerBinary,[switch]$FlatHud)
# Play Dying Light 2 in head-tracked stereo on the headset until the game exits, with the Rust viewer: this runs
# monaka_play, which starts the game through Steam unless it is running (or -NoLaunch), attaches the DL2 producer
# (dl2), shows it with monaka_viewer, and restarts both if a run ends. Run folders go to Monaka's runs folder.
# Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_dl2
# Requirements: SteamVR up with the headset connected; DirectX 12 recommended.
# Menus and loading screens show on a flat screen in front of you.
# Smooth is the default here (DirectX 12): each eye rendered every frame and an in-between frame generated per eye
# (FSR), about twice the rendered rate to the headset. -SameFrame renders each eye without the in-between frames
# (also DirectX 11). -DepthStereo: one render per frame, both eyes made from its depth (fastest; small gaps and
# doubled edges beside near things). -AlternateEye: one eye per frame, the eyes taking turns. The HUD comes from
# the game's own HUD draws, kept out of the frame and laid over each eye in front of you, the compass and the rest
# of the dynamic HUD on the hands, in every DirectX 12 mode; the game's Frame Generation setting does not matter.
# -FrameGen (DirectX 12): same-frame stereo with AMD FSR frame generation per eye, from the game's own FidelityFX
# DLLs: each eye's frame halfway between two rendered ones goes out between them, doubling the rate to the headset.
# Keep the game's own Frame Generation off.
# -HudScale (depth stereo): size of the HUD in front of you (1 = as on the monitor).
# In depth stereo the dynamic HUD puts the compass, objectives and health on the right and left hands (the weapon and
# stamina too); -FlatHud keeps the whole HUD in front of you.
# -Aim picks what aims (one of four, as the launcher's Aim):
#   Hand        (default) motion controls: the first-person arms and the weapon on the controllers, the weapon aiming
#               where it points (-AimHand right, or left); swing a melee weapon to hit, throw, draw the bow and reload
#               by hand. With that controller off or untracked, the head aims.
#   Controller  the controller aims (a ring in the headset marks where); the game animates the arms.
#   Head        the head aims: the character looks, aims and attacks where you look; the game animates the arms.
#   Mouse       the mouse and stick aim and turn as on a monitor; the head only moves the view.
# The older switches still work: -NoHeadAim is -Aim Mouse, -HeadAim -Aim Head, -HandAim -Aim Controller (-HandRig is the
# default anyway).
# With the hands on the controllers, a free hand's fingers follow the headset's finger tracking (-FingerBinary: each
# finger either open or a fist). With -Aim Mouse, -LockPitch blocks mouse/stick pitch.
# The game renders at the headset's eye size while VR runs (default; -GameResolution keeps the game's own size). The
# game applies the switch only while it has focus: click into it once VR starts.
# The session itself is monaka_launch (crates\launch): this script only turns its parameters into monaka_play options,
# passing every one so its defaults stay these (monaka_play list shows them).
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
$root=$MonakaRoot
function OnOff([bool]$on) { if ($on) { 'on' } else { 'off' } }

$play=Join-Path $root 'target\release\monaka_play.exe'
if (-not (Test-Path -LiteralPath $play)) { throw "Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_dl2 (missing $play)." }
$invariant=[Globalization.CultureInfo]::InvariantCulture
# -FrameGen wins over -SameFrame, both over -DepthStereo and -AlternateEye; with none, frame generation (auto).
$mode=if ($FrameGen) { 'framegen' } elseif ($SameFrame) { 'same' } elseif ($DepthStereo) { 'depth' } elseif ($AlternateEye) { 'alternate' } else { 'auto' }
# The older switches, strongest first (-NoHeadAim: no head aim, so no hand aim or hands either).
if ($NoHeadAim) { $Aim='Mouse' } elseif ($HeadAim) { $Aim='Head' } elseif ($HandAim) { $Aim='Controller' } elseif ($HandRig) { $Aim='Hand' }
$aim=@{ Hand='hand_rig'; Controller='controller'; Head='head'; Mouse='off' }[$Aim]
& $play dl2 "mode=$mode" "aim=$aim" "aim_hand=$AimHand" "render_size=$(if ($HeadsetResolution -or -not $GameResolution) { 'headset' } else { 'game' })" `
    "world_hud=$(OnOff (-not $FlatHud))" "hud_scale=$($HudScale.ToString($invariant))" "lock_pitch=$(OnOff $LockPitch)" "finger_binary=$(OnOff $FingerBinary)" `
    'extra=' "launch=$(OnOff (-not $NoLaunch))" "settle_seconds=$SettleSeconds"
exit $LASTEXITCODE
