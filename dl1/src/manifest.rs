//! Dying Light's game info, for the launcher and the loader (`monaka_core::manifest`). Its options
//! are what the player holds, then those of the components ([`crate::Dl1`],
//! `monaka_producer::game::options`), then the launcher's own for starting it through Steam; its
//! how-tos ([`CONTROLS`]) say how each thing is done in VR.

use crate::Dl1;
use monaka_core::manifest::*;
use monaka_producer::game::{options, options_len};

const COMPONENTS: usize = options_len::<Dl1>("dl1");
const DL1: [Opt; COMPONENTS + 3] = {
    let components: [Opt; COMPONENTS] = options::<Dl1, COMPONENTS>("dl1");
    let mut all = [LAUNCH; COMPONENTS + 3];
    all[0] = PLAY_WITH;
    let mut i = 0;
    while i < COMPONENTS {
        all[i + 1] = components[i];
        i += 1;
    }
    all[COMPONENTS + 2] = settle(120.0, 20.0);
    all
};

const VR: (&str, &str) = ("play_with", plays::VR);
const MOTION: (&str, &str) = ("aim", aims::HAND_RIG);

/// How each thing is done in Dying Light in VR, for the Controls tab.
const CONTROLS: &[HowTo] = &[
    HOW_BUTTONS,
    HOW_WALK,
    HowTo { action: "Melee", ways: &[way("Blades only cut edge-first.", &[MOTION])], options: &[] },
    HowTo {
        action: "Reload",
        ways: &[way("Touch the gun with your left hand, then pull away.", &[MOTION, ("vr_buttons", "1")]), way("Press X.", &[VR])],
        options: &[],
    },
    HowTo {
        action: "Crouch, heal, flashlight",
        ways: &[
            way("Duck to crouch.", &[("vr_buttons", "1")]),
            way("Y: tap to heal, hold to repair. B: flashlight.", &[VR, ("vr_buttons", "1")]),
            way("B: crouch. D-pad down: heal. D-pad up: flashlight.", &[VR, ("vr_buttons", "0")]),
        ],
        options: &["vr_buttons"],
    },
    HowTo {
        action: "Tool and weapon wheels",
        ways: &[
            way("Hold a hand over its shoulder (left: tools, right: weapons), flick that hand's stick to pick.", &[VR, ("dpad_hold", "1"), ("menu_reach", "same")]),
            way("Hold a hand over its shoulder (left: tools, right: weapons), flick the other hand's stick to pick.", &[VR, ("dpad_hold", "1"), ("menu_reach", "other")]),
            way("D-pad left (tools) or right (weapons), then A, B, X or Y.", &[VR, ("dpad_hold", "1")]),
            way("Hold d-pad left (tools) or right (weapons) and press A, B, X or Y.", &[VR, ("dpad_hold", "0")]),
        ],
        options: &["dpad_hold", "menu_reach", "menu_reach_depth"],
    },
    HowTo { action: "Throw", ways: &[way("Hold the left trigger, throw, let go.", &[MOTION])], options: &[] },
    HowTo { action: "Bow", ways: &[way("Bow in the left hand, draw with the right.", &[MOTION])], options: &[] },
    HowTo { action: "Lockpicking", ways: &[way("Twist the left hand for the pick, the right for the screwdriver.", &[MOTION])], options: &[] },
    HowTo { action: "Zombie grabs", ways: &[], options: &["disable_zombie_grabs"] },
];

pub const MANIFESTS: &[Manifest] = &[Manifest {
    id: "dl1",
    name: "Dying Light",
    status: Status::Alpha,
    executables: &["DyingLightGame.exe"],
    steam_app: 239140,
    producer: "monaka_dl1",
    log: "monaka_dl1.log",
    options_file: "dl1-options.txt",
    run_prefix: "dl1",
    launch: Launch::Auto { settle_seconds: 20 },
    refused_arguments: &[("/3dtv", "Remove /3dtv from the game's launch options in Steam; VR uses the normal renderer.")],
    before: "In the game's video options, turn off NVIDIA HBAO+, depth of field and PCSS shadows (in VR they make shadows slide). If the game crashes, reset its resolution.",
    options: &DL1,
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
    controls: CONTROLS,
}];
