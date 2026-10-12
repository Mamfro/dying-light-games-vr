//! The controllers as the game's pad, with the wheels held open by the d-pad or reached for over a
//! shoulder.

use crate::{Dl1, stereo};
use monaka_core::manifest::{CONTROLLER_PAD, CONTROLLER_PROXY, MENU_REACH_DEPTH, Opt, dpad_hold, menu_reach};
use monaka_producer::game::Component;

/// The tools (left) and weapons (right) stay open while the d-pad is held; picked with A, B, X, Y.
const DPAD_HOLD: Opt = dpad_hold(&["--dpad-hold", "left,right"]);
const MENU_REACH: Opt = menu_reach(
    "Rest your left hand over your left shoulder for the tools, your right hand over your right shoulder for the weapons. Flick a stick toward the one you want; bring the hand back to close.",
);

/// The controllers as the game's pad, the whole right stick to the game (head aim owns the facing).
/// `pad_mask_real=1` (a diagnostic) drops a real pad's input.
pub const GAMEPAD: Component<Dl1> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_REACH, MENU_REACH_DEPTH, CONTROLLER_PROXY],
    needs: &[&stereo::PRESENT],
    arm: arm_pad,
    ..monaka_pad::feature::gamepad()
};

fn arm_pad() {
    monaka_pad::feature::whole_stick();
    if stereo::eyes::config().pad_mask_real {
        monaka_pad::mask_real(true);
    }
}
