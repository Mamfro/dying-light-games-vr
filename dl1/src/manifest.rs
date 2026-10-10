//! What this game crate declares about itself, for the launcher, the loader and its own config
//! (`monaka_core::manifest`). Built from that module alone: the launcher compiles this file too.

use monaka_core::manifest::*;

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "dl1",
    name: "Dying Light",
    status: Status::Beta,
    executables: &["DyingLightGame.exe"],
    steam_app: 239140,
    producer: "monaka_dl1",
    log: "monaka_dl1.log",
    options_file: "dl1-options.txt",
    run_prefix: "dl1",
    launch: Launch::Auto { settle_seconds: 20 },
    refused_arguments: &[("/3dtv", "Remove /3dtv from the game's launch options in Steam; VR uses the normal renderer.")],
    before: "In the game's video options, turn off NVIDIA HBAO+, depth of field and PCSS shadows (in VR they make shadows slide).",
    options: &[
        aim(&[AIM_MOTION, AIM_LOOK, AIM_POINT, AIM_MOUSE], aims::HAND_RIG),
        AIM_HAND,
        render_size("If the game crashes, reset its resolution."),
        WORLD_HUD,
        HUD_SIZE.default_number(0.36),
        HUD_DISTANCE,
        FINGER_TRACKING,
        opt("room_scale", "Room-scale movement", "Your real steps and leans move your character; walls and enemies still stop it.", Kind::Switch { default: true })
            .on(Shown::Game)
            .only_when("aim", "hand_rig"),
        opt("disable_zombie_grabs", "Disable zombie grabs", "Zombies never grab you. Off: they grab and bite until you break free, as in the game.", Kind::Switch { default: true })
            .on(Shown::Game),
        BINARY_FINGERS,
        opt("fsr", "FSR upscaling", "Renders each eye smaller, then upscales it (needs the headset resolution and Dying Light 2's FidelityFX DLLs).", Kind::Switch { default: false })
            .on(Shown::Engine)
            // Upscales to the eye size, with the FidelityFX runtime Dying Light 2 installs (Monaka ships none).
            .needs_eye_size()
            .needs_steam_files(SteamFiles {
                game_folder: "Dying Light 2",
                path: r"ph\work\bin\x64",
                file: "amd_fidelityfx_upscaler_dx12.dll",
                line: "fsr_dlls",
                missing: "FSR is off: it needs AMD's FidelityFX DLLs, which come with Dying Light 2 (not installed).",
            }),
        opt("fsr_scale", "FSR quality", "How much smaller each eye renders.", choice(&[("1.5", "Quality"), ("1.7", "Balanced"), ("2", "Performance"), ("3", "Ultra performance")], "1.5"))
            .on(Shown::Engine)
            .only_when("fsr", "1"),
        opt("fsr_sharpness", "FSR sharpness", "0 turns sharpening off.", Kind::Number { min: 0.0, max: 1.0, step: 0.05, default: 0.3 }).on(Shown::Engine).only_when("fsr", "1"),
        EYE_SEPARATION,
        // The tools (left) and weapons (right) stay open while the d-pad is held; picked with A, B, X, Y.
        dpad_hold(&["--dpad-hold", "left,right"]),
        menu_reach("Rest your left hand over your left shoulder for the tools, your right hand over your right shoulder for the weapons. Flick a stick toward the one you want; bring the hand back to close."),
        MENU_REACH_DEPTH,
        CONTROLLER_PROXY, OTHER_BUILDS, EXTRA,
        LAUNCH,
        settle(120.0, 20.0),
    ],
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
}];
