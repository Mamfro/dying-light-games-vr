//! The controllers as the game's pad.

use crate::Dl2;
use monaka_core::manifest::{CONTROLLER_PAD, CONTROLLER_PROXY};
use monaka_producer::game::Component;

/// The controllers as the game's pad, the whole right stick to the game (head aim and the pitch
/// lock decide what its pitch does).
pub const GAMEPAD: Component<Dl2> = Component { options: &[CONTROLLER_PAD, CONTROLLER_PROXY], arm: monaka_pad::feature::whole_stick, ..monaka_pad::feature::gamepad() };
