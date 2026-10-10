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
        opt("motion_throw", "Throw by hand", "Throw knives, bombs and carried things with the hand holding them, and a melee weapon with your right hand: they fly the way and as hard as your hand moved.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        opt("motion_bow", "Bow by hand", "Hold fire and pull the string hand back from the bow hand: how far you pull sets the power, and the arrow flies along the line from the string through the bow.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        opt("manual_reload", "Reload by hand", "A gun reloads only while your left hand is at it: bring the hand to the gun to work the magazine or feed the rounds.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        opt("motion_lockpick", "Lockpick by hand", "Twist your left wrist to turn the pick around the lock and your right wrist to turn the screwdriver.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        opt("disable_zombie_grabs", "Disable zombie grabs", "Biters never grab you from the front. Off: they grab and bite until you break free, as in the game.", Kind::Switch { default: true })
            .on(Shown::Game),
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
