//! The controllers as the game's pad, with the wheels held open by the d-pad.

use crate::{Beast, stereo};
use monaka_core::manifest::{CONTROLLER_PAD, CONTROLLER_PROXY, Kind, Opt, Shown, dpad_hold, line};
use monaka_producer::game::Component;

/// The tools (left) and weapons (right) wheels stay open while the d-pad is held, picked with A, B,
/// X, Y; the viewer holds them for a hand reached over that shoulder (its default). Part of the
/// controls, not a setting.
const DPAD_HOLD: Opt = dpad_hold(&["--dpad-hold", "left,right"]).on(Shown::Line);
/// The wheels pick with the right stick (not the face buttons): the reaching hand's flick goes to
/// the game as that stick.
const MENU_PICK_STICK: Opt = line("menu_pick_stick", "Wheels picked by stick", "The reaching hand's flick reaches the game as its right stick.", Kind::Switch { default: true })
    .launcher()
    .viewer_args("1", &["--menu-pick", "stick"]);

/// The controllers as the game's pad, the whole right stick to the game (its look steers toward
/// targets; head aim neutralises the stick's pitch itself).
pub const GAMEPAD: Component<Beast> = Component {
    options: &[CONTROLLER_PAD, DPAD_HOLD, MENU_PICK_STICK, CONTROLLER_PROXY],
    needs: &[&stereo::EYES],
    arm: monaka_pad::feature::whole_stick,
    ..monaka_pad::feature::gamepad()
};
