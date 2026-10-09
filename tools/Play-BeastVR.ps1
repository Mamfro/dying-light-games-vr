[CmdletBinding()]
param([ValidateRange(0,8)][int]$Latency=2,[ValidateRange(0.04,0.09)][double]$EyeSeparation=0.064,[switch]$HeadsetResolution,[switch]$GameResolution,
    [ValidatePattern('^\d+x\d+$')][string]$EyeSize,
    [switch]$FrameGen,[switch]$NoHeadAim,[switch]$HandAim,[switch]$HandRig,[ValidateSet('left','right')][string]$AimHand='right',[switch]$FingerBinary,[switch]$FlatHud,
    [ValidatePattern('^[a-z_0-9]+=[\w.,:\-]+$')][string[]]$Experiment=@())
# Play Dying Light: The Beast in head-tracked stereo on the headset until the game exits, with the Rust viewer: this runs
# monaka_play, which attaches the Beast producer (beast) to the running game, shows it with monaka_viewer, and
# restarts both if a run ends. Run folders go to Monaka's runs folder.
# Build first: cargo build --release -p monaka -p monaka_viewer -p monaka_beast
# Requirements: SteamVR up with the headset connected; the game running on DirectX 12 with its own Frame Generation and
# motion blur off.
# Alternate-eye stereo: each rendered frame is one eye, so the headset gets half the game's frame rate per eye. The view
# keeps the game's field of view; the head turns and moves it on top of the mouse/stick look.
# -EyeSeparation is used when the headset reports no IPD.
# The game renders at the headset's eye size (windowed) while VR runs (default; -GameResolution keeps the game's own size), so no work goes into pixels the headset
# never shows; your own mode and size come back when VR stops. The size is SteamVR's recommended one (it includes
# SteamVR's supersampling); -EyeSize WxH asks for a size of your own instead (2160x2160 is the Steam Frame's panel; 256
# to 8192 each way). The game applies the switch only while it has focus (until then the headset shows nothing; after
# 15 s of nothing the run restarts by itself): click into it as VR starts. If the game keeps its own size, VR goes on at
# that size.
# Head aim (default): the character looks, aims and attacks where the head points; -NoHeadAim leaves it on the mouse/stick.
# -HandAim aims with a controller instead (-AimHand left|right); -HandRig also puts the first-person arms on the controllers
# (swinging a melee weapon attacks, and melee aims with the head). Hand aim and the arms need head aim.
# -FrameGen: per-eye FSR frame generation, each eye's frame of the other eye's moment generated, so both eyes change
# every frame (keep the game's own frame generation and motion blur off).
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
# Head aim carries hand aim and the arms, so -NoHeadAim wins; -HandRig implies -HandAim.
$aim=if ($NoHeadAim) { 'off' } elseif ($HandRig) { 'hand_rig' } elseif ($HandAim) { 'controller' } else { 'head' }
& $play beast "render_size=$render" "eye_size=$EyeSize" "latency=$Latency" "eye_separation=$($EyeSeparation.ToString($invariant))" `
    "aim=$aim" "aim_hand=$AimHand" "world_hud=$(if ($FlatHud) { 'off' } else { 'on' })" "finger_binary=$(if ($FingerBinary) { 'on' } else { 'off' })" "mode=$(if ($FrameGen) { 'framegen' } else { 'standard' })" "extra=$($Experiment -join ' ')"
exit $LASTEXITCODE
