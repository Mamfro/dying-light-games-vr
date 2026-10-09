//! What this game crate declares about itself, for the launcher, the loader and its own config
//! (`monaka_core::manifest`). Built from that module alone: the launcher compiles this file too.

use monaka_core::manifest::*;

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "beast",
    name: "Dying Light: The Beast",
    status: Status::Experimental,
    executables: &["DyingLightGame_TheBeast_x64_rwdi.exe"],
    steam_app: 3008130,
    producer: "monaka_beast",
    log: "monaka_beast.log",
    options_file: "beast-options.txt",
    run_prefix: "beast",
    launch: Launch::Manual,
    refused_arguments: &[],
    before: "Use DirectX 12 with the game's own Frame Generation and motion blur off. Start the game and load into gameplay first.",
    options: &[
        // Look-based by default: motion controls here have had little testing.
        aim(&[AIM_MOTION, AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HEAD),
        AIM_HAND,
        render_size("The headset's size runs much better.").eye_size_note("Click into the game: it switches to {size} once it has focus, and VR shows it from then on."),
        WORLD_HUD,
        FINGER_TRACKING,
        opt("room_scale", "Room-scale movement", "Your real steps and leans move your character; walls and enemies still stop it.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        BINARY_FINGERS,
        opt("mode", "Rendering", "Smooth adds in-between frames.", choice(&[("standard", "Standard"), ("framegen", "Smooth")], "standard")).on(Shown::Engine),
        line("eye_size", "Render size (WxH)", "A render size of your own; replaces render_size.", Kind::Size).launcher(),
        line("stereo", "Stereo", "Off: the probes only, the game as it plays on the monitor.", Kind::Switch { default: true }),
        LATENCY,
        EYE_SEPARATION,
        CONTROLLER_PROXY, OTHER_BUILDS, EXTRA,
    ],
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
}];
