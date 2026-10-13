use super::*;
use monaka_core::pad::button;
use monaka_pad::actions::buttons;

#[test]
fn b_is_the_flashlight_and_a_duck_taps_the_crouch_toggle() {
    assert_eq!(<Beast as Remaps>::REMAPS, [Remap { from: button::B, to: button::DPAD_DOWN }]);
    assert_eq!(<Beast as Actions>::binding(Action::Crouch), Binding::Toggle(buttons(button::B)));
    assert_eq!(<Beast as Actions>::binding(Action::Flashlight), Binding::Pad(buttons(button::DPAD_DOWN)));
}
