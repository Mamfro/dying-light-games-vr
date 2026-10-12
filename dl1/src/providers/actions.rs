//! Provider: actions (`monaka_pad::actions::Actions`: a game action pressed or held by what it
//! means).
//!
//! On the pad: the light attack on the right trigger (`eng_chr::providers::actions`), a throw on
//! the left trigger (a tool leaves as it is let go), the flashlight on the d-pad's up; a heal on
//! its down and a reload on X, and crouching as the game's duck toggle on B, while the VR button
//! layout is off. With that layout in (`extras::vr_buttons`), three go the game's own way, through
//! the input manager's pad button event on codes no controller sends: crouching held for as long as it
//! is asked (the hold-to-crouch action, which no pad button has), a heal and (with the reload
//! gesture's binding) a reload pressed once. The ask is kept here and pressed on the engine's next
//! input pass, beside the layout's own presses.

use crate::Dl1;
use crate::extras::vr_buttons::{APPLYING, RELOAD_GESTURE};
use monaka_core::pad::{Gamepad, button};
use monaka_pad::actions::{Action, Actions, Ask, Binding, buttons};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Crouching asked held; a heal and a reload asked, until the input pass presses them.
pub(crate) static CROUCH_ASKED: AtomicBool = AtomicBool::new(false);
pub(crate) static HEAL_ASKED: AtomicBool = AtomicBool::new(false);
pub(crate) static RELOAD_ASKED: AtomicBool = AtomicBool::new(false);

impl Actions for Dl1 {
    /// The lockpick's pick and turn the game's own way once its minigame is hooked
    /// (`crate::providers::lockpick`); never the sticks, which outside the minigame move the
    /// character.
    fn binding(action: Action) -> Binding {
        match action {
            Action::Pick | Action::Turn if crate::providers::lockpick::installed() => Binding::Engine,
            _ => binding(action, APPLYING.load(Relaxed), RELOAD_GESTURE.load(Relaxed)),
        }
    }

    /// Kept for the input pass: crouching held or let go, a heal or a reload pressed once (the
    /// layout's pulse, whatever the asked length). The pick and the turn followed are read by the
    /// minigame itself.
    fn engine(action: Action, ask: Ask) -> bool {
        let (flag, on) = match (action, ask) {
            (Action::Crouch, Ask::Hold) => (&CROUCH_ASKED, true),
            (Action::Crouch, Ask::LetGo) => (&CROUCH_ASKED, false),
            (Action::Heal, Ask::Tap { .. }) => (&HEAL_ASKED, true),
            (Action::Reload, Ask::Tap { .. }) => (&RELOAD_ASKED, true),
            (Action::Pick | Action::Turn, Ask::Follow | Ask::LetGo) => return crate::providers::lockpick::installed(),
            _ => return false,
        };
        flag.store(on, Relaxed);
        true
    }
}

/// How Dying Light 1 takes `action`, with the VR button layout in (`layout`) and the reload
/// gesture's button bound (`reload_key`).
fn binding(action: Action, layout: bool, reload_key: bool) -> Binding {
    match action {
        Action::Crouch if layout => Binding::Engine,
        Action::Crouch => Binding::Toggle(buttons(button::B)),
        Action::Heal if layout => Binding::Engine,
        Action::Heal => Binding::Pad(buttons(button::DPAD_DOWN)),
        Action::Reload if layout && reload_key => Binding::Engine,
        Action::Reload => Binding::Pad(buttons(button::X)),
        Action::Flashlight => Binding::Pad(buttons(button::DPAD_UP)),
        Action::Throw => Binding::Pad(Gamepad { left_trigger: 255, ..Gamepad::default() }),
        _ => eng_chr::providers::actions::binding(action),
    }
}

/// Every ask forgotten (the layout's stop).
pub(crate) fn forget() {
    for flag in [&CROUCH_ASKED, &HEAL_ASKED, &RELOAD_ASKED] {
        flag.store(false, Relaxed);
    }
}

/// A heal asked, taken by the input pass that presses it.
pub(crate) fn take_heal() -> bool {
    HEAL_ASKED.swap(false, Relaxed)
}

/// A reload asked, taken by the input pass that presses it.
pub(crate) fn take_reload() -> bool {
    RELOAD_ASKED.swap(false, Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_takes_crouch_heal_and_reload_its_own_way() {
        assert_eq!(binding(Action::Crouch, true, false), Binding::Engine);
        assert_eq!(binding(Action::Crouch, false, false), Binding::Toggle(buttons(button::B)), "the game's duck toggle");
        assert_eq!(binding(Action::Heal, true, false), Binding::Engine);
        assert_eq!(binding(Action::Heal, false, false), Binding::Pad(buttons(button::DPAD_DOWN)));
        assert_eq!(binding(Action::Reload, true, false), Binding::Pad(buttons(button::X)), "no gesture button bound: X");
        assert_eq!(binding(Action::Reload, true, true), Binding::Engine);
        assert_eq!(binding(Action::Attack, true, true), Binding::Pad(Gamepad { right_trigger: 255, ..Gamepad::default() }));
        assert_eq!(binding(Action::Jump, true, true), Binding::None);
    }

    #[test]
    fn asks_wait_for_the_input_pass() {
        forget();
        assert!(<Dl1 as Actions>::engine(Action::Heal, Ask::Tap { ms: 100 }));
        assert!(take_heal() && !take_heal(), "taken once");
        assert!(<Dl1 as Actions>::engine(Action::Crouch, Ask::Hold));
        assert!(CROUCH_ASKED.load(Relaxed));
        assert!(<Dl1 as Actions>::engine(Action::Crouch, Ask::LetGo));
        assert!(!CROUCH_ASKED.load(Relaxed));
        assert!(!<Dl1 as Actions>::engine(Action::Heal, Ask::Hold), "a heal is a press");
        assert!(!<Dl1 as Actions>::engine(Action::Jump, Ask::Tap { ms: 100 }));
        forget();
    }
}
