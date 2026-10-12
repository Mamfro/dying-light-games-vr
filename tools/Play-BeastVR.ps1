[CmdletBinding()]
param([ValidateRange(0,8)][int]$Latency=2,[ValidateRange(0.04,0.09)][double]$EyeSeparation=0.064,[switch]$HeadsetResolution,[switch]$GameResolution,
    [ValidatePattern('^\d+x\d+$')][string]$EyeSize,
    [ValidateSet('aer','afw')][string]$Stereo='aer',[ValidateSet('off','fsr')][string]$FrameGen='off',[ValidateSet('Hand','Controller','Head','Mouse')][string]$Aim='Hand',[switch]$HeadAim,[switch]$NoHeadAim,[switch]$HandAim,[switch]$HandRig,[ValidateSet('left','right')][string]$AimHand='right',[switch]$FingerBinary,[switch]$FlatHud,
    [ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# Play Dying Light: The Beast in head-tracked stereo on the headset until the game exits, with the Rust viewer: this runs
# monaka_play, which attaches the Beast producer (beast) to the running game, shows it with monaka_viewer, and
# restarts both if a run ends. Run folders go to Monaka's runs folder.
# Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_beast
# Requirements: SteamVR up with the headset connected; the game running on DirectX 12 with its own Frame Generation and
# motion blur off.
# -Stereo aer (default, AER): each rendered frame is one eye, so the headset gets half the game's frame rate per eye.
# -Stereo afw (AFW): each frame also warped into the other eye with its depth, so both eyes change every frame. The view
# keeps the game's field of view; the head turns and moves it on top of the mouse/stick look.
# -EyeSeparation is used when the headset reports no IPD.
# The game renders at the headset's eye size (windowed) while VR runs (default; -GameResolution keeps the game's own size), so no work goes into pixels the headset
# never shows; your own mode and size come back when VR stops. The size is SteamVR's recommended one (it includes
# SteamVR's supersampling); -EyeSize WxH asks for a size of your own instead (2160x2160 is the Steam Frame's panel; 256
# to 8192 each way). The game applies the switch only while it has focus (until then the headset shows nothing; after
# 15 s of nothing the run restarts by itself): click into it as VR starts. If the game keeps its own size, VR goes on at
# that size.
# -Aim picks what aims (one of four, as the launcher's Aim):
#   Hand        (default) motion controls: the first-person arms and the weapon on the controllers, the weapon aiming
#               where it points (-AimHand right, or left); swing a melee weapon to hit, throw, draw the bow and reload
#               by hand. With that controller off or untracked, the head aims.
#   Controller  the controller aims (a ring in the headset marks where); the game animates the arms.
#   Head        the head aims: the character looks, aims and attacks where you look; the game animates the arms.
#   Mouse       the mouse and stick aim and turn as on a monitor; the head only moves the view.
# The older switches still work: -NoHeadAim is -Aim Mouse, -HeadAim -Aim Head, -HandAim -Aim Controller (-HandRig is the
# default anyway).
# -FrameGen fsr: FSR frame generation (default off). With AER each eye's frame of the other eye's moment is generated,
# so both eyes change every frame; with AFW in-between pairs go out between the warped ones (keep the game's own frame
# generation and motion blur off).
# -Experiment passes extra producer options (key=value; see beast\src\lib.rs).
# The session itself is monaka_launch (crates\launch): this script only turns its parameters into monaka_play options,
# passing every one so its defaults stay these (monaka_play list shows them).
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Monaka.ps1')
$play=Join-Path $MonakaRoot 'target\release\monaka_play.exe'
if (-not (Test-Path -LiteralPath $play)) { throw "Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_beast (missing $play)." }
$invariant=[Globalization.CultureInfo]::InvariantCulture
function OnOff([bool]$on) { if ($on) { 'on' } else { 'off' } }
# -EyeSize wins over both resolution switches; -HeadsetResolution wins over -GameResolution.
$render=if ($HeadsetResolution -or -not $GameResolution) { 'headset' } else { 'game' }
# The older switches, strongest first (-NoHeadAim: no head aim, so no hand aim or hands either).
if ($NoHeadAim) { $Aim='Mouse' } elseif ($HeadAim) { $Aim='Head' } elseif ($HandAim) { $Aim='Controller' } elseif ($HandRig) { $Aim='Hand' }
$aim=@{ Hand='hand_rig'; Controller='controller'; Head='head'; Mouse='off' }[$Aim]
& $play beast "render_size=$render" "eye_size=$EyeSize" "latency=$Latency" "eye_separation=$($EyeSeparation.ToString($invariant))" `
    "aim=$aim" "aim_hand=$AimHand" "world_hud=$(if ($FlatHud) { 'off' } else { 'on' })" "finger_binary=$(if ($FingerBinary) { 'on' } else { 'off' })" "stereo=$Stereo" "framegen=$FrameGen" "extra=$($Experiment -join ' ')"
exit $LASTEXITCODE
