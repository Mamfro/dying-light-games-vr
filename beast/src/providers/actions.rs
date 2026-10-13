//! Provider: actions (`monaka_pad::actions::Actions`), the game's actions on the pad; its component
//! is `crate::ACTIONS`, the pad itself `crate::GAMEPAD`. And the VR button layout
//! (`monaka_pad::remap::Remaps`), its component `crate::CONTROLS`.

use crate::Beast;
use monaka_pad::actions::{Action, Actions, Ask, Binding};
use monaka_pad::remap::{Remap, Remaps};

/// The actions The Beast is known to take (`eng_chr::providers::actions`): on the pad as its default
/// layout has them (crouching on B, the flashlight on the d-pad's down), and the reload its own way.
impl Actions for Beast {
    fn binding(action: Action) -> Binding {
        eng_chr::providers::actions::binding_dl2(action)
    }

    fn engine(action: Action, ask: Ask) -> bool {
        eng_chr::providers::actions::engine(action, ask)
    }
}

/// The VR button layout Dying Light 2 and The Beast share (`eng_chr::providers::actions`): in play
/// the controllers' B is the flashlight.
impl Remaps for Beast {
    const REMAPS: &'static [Remap] = eng_chr::providers::actions::VR_BUTTONS;

    fn applies() -> bool {
        eng_chr::providers::actions::in_play()
    }
}

#[cfg(test)]
mod tests;
