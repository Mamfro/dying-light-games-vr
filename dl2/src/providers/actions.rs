//! Providers: actions (`monaka_pad::actions::Actions`), the game's actions on the pad; its component
//! is `crate::ACTIONS`, the pad itself `crate::GAMEPAD`. And lockpicking
//! (`monaka_body::lockpick::Lockpicks`), on the minigame the actions take the pick and the turn
//! through; its component is `crate::LOCKPICK`.

use crate::Dl2;
use monaka_body::lockpick::{Lockpicking, Lockpicks};
use monaka_pad::actions::{Action, Actions, Ask, Binding};
use monaka_producer::game::{Lack, Start};

/// The actions Dying Light 2 is known to take (`eng_chr::providers::actions`): on the pad, and the reload
/// and the lockpick's pick and turn its own way.
impl Actions for Dl2 {
    fn binding(action: Action) -> Binding {
        eng_chr::providers::actions::binding(action)
    }

    fn engine(action: Action, ask: Ask) -> bool {
        eng_chr::providers::actions::engine(action, ask)
    }
}

/// Lockpicking on the Chrome games' twist and minigame (`eng_chr::providers::lockpick`); the wrists
/// are the controllers' palms.
impl Lockpicks for Dl2 {
    const LOCKPICKING: Lockpicking = eng_chr::providers::lockpick::LOCKPICKING;

    fn picking() -> bool {
        eng_chr::providers::lockpick::picking()
    }

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        eng_chr::providers::lockpick::install_for(start)
    }
}
