# Dying Light VR

Play Dying Light, Dying Light 2 and Dying Light: The Beast in VR. Features:
* Your aim follows your controllers (each hand is controller by one VR controller)
* Each hand has a purpose (left = tools, right = weapons)
* The game's GUI exists on your hands instead of flat in front of you
* Attacks are physically based, so swinging at an arm will hit the arm and not just deliver a generic attack
* Dynamic attack damage types (a forwards slash does slashing damage, but a backhand hit without the blade does blunt instead)

This is a game pack for [Monaka VR](https://github.com/Mamfro/monaka_vr), which runs it.

> **Pre-alpha.** Expect bugs and missing pieces. Until Monaka's API settles, things change often,
> and a pack made for an older Monaka may stop working with a newer one.
>
> **Before you play,** 
> * In Dying Light, turn off NVIDIA HBAO+, depth of field and PCSS shadows (in VR they make shadows slide).
> * In Dying Light 2, and Dying Light: The Beast, use DX12.

## Games

| Game | Status | Controls | Aim | HUD |
|---|---|---|---|---|
| **Dying Light** | Alpha | Motion controls | Head, controller | Dynamic, game HUD |
| **Dying Light 2** | Pre-alpha | Motion controls | Head, controller | Dynamic, game HUD, monitor layout |
| **Dying Light: The Beast** | Experimental | Motion controls | Head, controller | Dynamic, monitor layout |

**Status Meaning**, from most to least finished:
- **Beta**: I'm confident that you can play the whole game this way. Small features/polish and deeper integration may be lacking.
- **Alpha**: I'm somewhat confident you can play the whole game this way, but I haven't played multiple hours at a time.
- **Pre-alpha**: It runs, but probably will be a bad experience. Very rough edges remain.
- **Experimental**: probably runs, but rough.

**Motion controls**: your VR controllers work as a gamepad, and your in-game hands and what they
hold follow the controllers. 

**Aim**: what points your weapon. **Head** means you aim where you look; **Controller** means you
aim where a controller points. Turning with the stick or mouse still works on top.

**HUD**: **Dynamic** puts pieces of the HUD on your hands. **Game HUD** is the game's own HUD, shown
in front of you. **Monitor layout** means it stays where it is on a monitor, so the edges can be
hard to see.

Currently only the **Steam** versions are supported. Online play and co-op are not supported.

## Setting up

1. Install **Monaka VR** from its [Releases page](https://github.com/Mamfro/monaka_vr/releases) (see
   its [README](https://github.com/Mamfro/monaka_vr)).
2. Download `dying-light-games-vr-<version>.zip` from this repository's
   [Releases page](https://github.com/Mamfro/dying-light-games-vr/releases). Don't unzip it.
3. In the Monaka VR window, press **Add game pack...** and pick the zip. All three games appear in
   the list.

Then play as Monaka VR's README says: pick the game, start SteamVR, press **Start VR**. To update,
add the newer zip the same way: it replaces the old one.


## Controllers

Your VR controllers act as one Xbox controller (see Monaka VR's README for the Quest and Index
d-pad). In Dying Light the tools (d-pad left) and weapons (d-pad right) stay open only while the
d-pad is held. On a Quest, a d-pad left or right from the thumbrest stays held for you until you've
picked with A, B, X or Y, so both thumbs are free. Push the same direction again to close it without
picking. "Hold d-pad menus" on the Game tab turns this off.

## What works

| | Dying Light | Dying Light 2 | The Beast |
|---|---|---|---|
| **Aim** | Head, controller. With a controller, melee and climbing still aim with your head. | Head, controller | Head, controller. Little tested. |
| **HUD** | Dynamic (default): minimap, quests and weapon on the right hand; health and quick item on the left; the rest in front of you. | Dynamic in every DirectX 12 mode: compass and weapon on the right hand, health and stamina on the left; objective, waypoint and loot markers at their targets' distance (not yet hidden by walls); the rest in front of you. Other modes use the monitor layout. | Dynamic in DirectX 12, as Dying Light 2 (compass and weapon on the right hand, health and stamina on the left), without the world markers. Not yet checked in a headset. With it off, the monitor layout. |
| **Hands** | Tracked. Not every weapon and item has been checked. | Tracked | Tracked. Little tested. |
| **Melee** | Swing to attack | Swing to attack | Swing to attack. |
| **Left hand** | Its own aim: grappling hook and throwables. | Its own aim: throwables and the other left-trigger accessories. Not yet checked in a headset. | As Dying Light 2. Not yet checked in a headset. |
| **Finger tracking** | On a free hand | On a free hand. | On a free hand. |
| **Cutscenes** | Mostly fine; some problems left | Flicker | Not reviewed |

**Swing to attack**: swinging the controller starts the game's own attack. Your swing doesn't steer
the blow, but it beats pressing a trigger.

## Known bugs

#### Dying Light
* Some animations may still cause a flickering effect
* Reaching behind your right shoulder to bring up the weapon wheel while holding a 2 handed rifle may move your character backwards

#### Dying Light 2 and The Beast
These have not been tested in a while. The shoulder-reach-for-weapon-wheel may not work correctly.

## When something goes wrong

| What you see | What to do |
|---|---|
| The headset stays black (The Beast) | Click into the game window so it can switch resolution. |

Everything else is in Monaka VR's README.

## For developers

To build it yourself, clone [Monaka VR](https://github.com/Mamfro/monaka_vr) as `monaka_vr` and this
repository beside it in a `monaka_adapters` folder, then run `cargo build --release` here:

```text
monaka_vr\
monaka_adapters\
    dying-light-games-vr\
```

The DLLs land in `target\release`; **Add game pack...** takes a single DLL too. `tools\Package.ps1`
makes the release zip in `dist\`.

`tools\` also has the scripts used while building this: `Play-*.ps1` start a session with each game's
usual settings, and the `Harness-*`, `Capture-*` and `Probe-*` scripts run checks on the PC without a
headset. They run Monaka's `monaka_play` from the `monaka_vr` folder beside `monaka_adapters` (or
`MONAKA_ROOT`). To build the DLLs straight into Monaka's own `target` (beside its launcher), copy
`cargo-config.example.toml` to `.cargo\config.toml`.

## Licence

Free software under the [GNU General Public License v3.0](LICENSE), like Monaka VR.

Dying Light 2 support builds on addresses and code found by
[farmerarmor's DyingLight2VR](https://github.com/farmerarmor/DyingLight2VR), used under the MIT
licence ([notice](licenses/DyingLight2VR-LICENSE.txt)).
