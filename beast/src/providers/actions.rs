//! Provider: actions (`monaka_pad::actions::Actions`), the game's actions on the pad; its component
//! is `crate::ACTIONS`, the pad itself `crate::GAMEPAD`.

use crate::Beast;
use monaka_pad::actions::{Action, Actions, Ask, Binding};

/// The actions The Beast is known to take (`eng_chr::providers::actions`): on the pad, and the reload
/// its own way.
impl Actions for Beast {
    fn binding(action: Action) -> Binding {
        eng_chr::providers::actions::binding(action)
    }

    fn engine(action: Action, ask: Ask) -> bool {
        eng_chr::providers::actions::engine(action, ask)
    }
}
