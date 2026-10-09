//! What this game crate declares about itself, for the launcher, the loader and its own config
//! (`monaka_core::manifest`). Built from that module alone: the launcher compiles this file too.

use monaka_core::manifest::*;

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "dl2",
    name: "Dying Light 2",
    status: Status::Beta,
    executables: &["DyingLightGame_x64_rwdi.exe"],
    steam_app: 534380,
    producer: "monaka_dl2",
    log: "monaka_dl2.log",
    options_file: "dl2-options.txt",
    run_prefix: "dl2",
    launch: Launch::Auto { settle_seconds: 30 },
    refused_arguments: &[],
    before: "In the game's options, use DirectX 12. The game's own Frame Generation can be on or off (Monaka VR holds it off while VR runs); for Smooth, turn it OFF.",
    options: &[
        aim(&[AIM_MOTION, AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HAND_RIG),
        AIM_HAND,
        render_size("Experimental in this game."),
        WORLD_HUD,
        opt("world_markers", "Markers in the world", "Objective and waypoint markers sit at their targets' distance instead of in the flat HUD.", Kind::Switch { default: true }).on(Shown::Hud),
        HUD_SIZE.default_number(0.55),
        FINGER_TRACKING,
        opt("room_scale", "Room-scale movement", "Your real steps and leans move your character; walls and enemies still stop it.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        BINARY_FINGERS,
        opt("mode", "Rendering", "Smooth renders each eye and adds in-between frames, Sharp renders each eye, Fast draws once for both eyes (gaps beside near things).", Kind::Choice {
            choices: &[("framegen", "Smooth"), ("same", "Sharp"), ("depth", "Fast"), ("alternate", "Alternate eye"), ("auto", "Smooth")],
            default: "framegen",
            window: 3,
        })
        .on(Shown::Engine)
        // Fast shows the hands no nearer than its nearest comfortable depth: the panels on them too.
        .viewer_args("depth", &["--panel-nearest", "0.5"]),
        line("lock_pitch", "Block stick pitch", "With mouse / stick aim: no looking up and down with the stick, except toward level.", Kind::Switch { default: false }),
        CONTROLLER_PROXY, OTHER_BUILDS, EXTRA,
        LAUNCH,
        settle(180.0, 30.0),
    ],
    licences: &["DyingLight2VR-LICENSE.txt"],
    left_hand_aims: true,
    weapon_ring: true,
}];
