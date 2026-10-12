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
    HowTo { action: "Look and turn", ways: &[way("Look around with your head; push the right stick to turn.", &[VR])], options: &[] },
    HOW_WALK,
    HowTo {
        action: "Fight",
        ways: &[
            way("Swing your weapon or your fists: the hit lands where they are, and a blade cuts only when its edge leads.", &[MOTION]),
            way("Point a gun with your hand and pull the right trigger: the shot leaves the barrel.", &[MOTION]),
        ],
        options: &[],
    },
    HowTo {
        action: "Reload",
        ways: &[
            way("Bring your left hand to the gun and take it away again.", &[MOTION, ("vr_buttons", "1")]),
            way("Or press X.", &[VR]),
        ],
        options: &[],
    },
    HowTo {
        action: "Crouch, heal and the flashlight",
        ways: &[
            way("Duck in your room to crouch; stand up to stop.", &[("vr_buttons", "1")]),
            way("Tap Y to heal; hold Y to repair your weapon. B turns the flashlight on and off.", &[VR, ("vr_buttons", "1")]),
            way("As on a gamepad: B crouches, d-pad down heals, d-pad up is the flashlight.", &[VR, ("vr_buttons", "0")]),
        ],
        options: &["vr_buttons"],
    },
    HowTo {
        action: "Tools and weapons wheels",
        ways: &[
            way(
                "Reach over your left shoulder for tools, over your right for weapons, and hold your hand still: the wheel opens. Flick that hand's stick toward what you want; take your hand away to close it.",
                &[VR, ("dpad_hold", "1"), ("menu_reach", "same")],
            ),
            way(
                "Reach over your left shoulder for tools, over your right for weapons, and hold your hand still: the wheel opens. Flick the other hand's stick toward what you want; take your hand away to close it.",
                &[VR, ("dpad_hold", "1"), ("menu_reach", "other")],
            ),
            way("Or push the d-pad left (tools) or right (weapons): the wheel stays open until you pick with A, B, X or Y.", &[VR, ("dpad_hold", "1")]),
            way("Hold the d-pad left (tools) or right (weapons) and pick with A, B, X or Y while you hold it.", &[VR, ("dpad_hold", "0")]),
        ],
        options: &["dpad_hold", "menu_reach", "menu_reach_depth"],
    },
    HowTo {
        action: "Throw",
        ways: &[way("Hold the left trigger, throw with your left hand and let go of the trigger: it flies the way your hand moved.", &[MOTION])],
        options: &[],
    },
    HowTo {
        action: "Bow",
        ways: &[way("Hold the bow in your left hand and pull the string back with your right: how far you pull sets the shot, and the arrow flies along the bow.", &[MOTION])],
        options: &[],
    },
    HowTo {
        action: "Lockpicking",
        ways: &[way("Twist your left hand to turn the pick and your right hand to turn the screwdriver.", &[MOTION])],
        options: &[],
    },
    HowTo { action: "Zombie grabs", ways: &[], options: &["disable_zombie_grabs"] },
];

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
    before: "In the game's video options, turn off NVIDIA HBAO+, depth of field and PCSS shadows (in VR they make shadows slide). If the game crashes, reset its resolution.",
    options: &DL1,
    licences: &[],
    left_hand_aims: true,
    weapon_ring: true,
    controls: CONTROLS,
}];
