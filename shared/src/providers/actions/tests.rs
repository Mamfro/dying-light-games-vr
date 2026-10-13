use super::*;
use monaka_body::held::Kind;
use monaka_body::reload::{Reloading, gestured};
use monaka_core::gesture::{TouchAway, TouchAwayRules};
use monaka_pad::actions::standard;
use std::cell::Cell;

#[test]
fn the_attack_is_the_trigger_a_swing_pressed() {
    assert_eq!(binding_with(Action::Attack, false), standard(Action::Attack));
    assert!(Action::ALL.iter().filter(|&&a| a != Action::Attack).all(|&a| binding_with(a, false) == Binding::None));
}

#[test]
fn the_reload_is_the_games_own_once_the_gun_is_hooked() {
    assert_eq!(binding_with(Action::Reload, true), Binding::Engine);
    assert!(Action::ALL.iter().filter(|&&a| a != Action::Attack && a != Action::Reload).all(|&a| binding_with(a, true) == Binding::None));
    if !HOOKED.load(Acquire) {
        assert!(!engine(Action::Reload, Ask::Tap { ms: 100 }), "not hooked: refused");
    }
    assert!(!engine(Action::Attack, Ask::Tap { ms: 100 }));
}

#[test]
fn dying_light_2_and_the_beast_crouch_on_b_and_toggle_the_flashlight_on_the_dpads_down() {
    assert_eq!(binding_dl2(Action::Crouch), Binding::Toggle(buttons(button::B)), "the game's crouch toggle");
    assert_eq!(binding_dl2(Action::Flashlight), Binding::Pad(buttons(button::DPAD_DOWN)));
    assert_eq!(binding_dl2(Action::Attack), binding(Action::Attack), "the rest as the three games share them");
    assert_eq!(binding_dl2(Action::Jump), Binding::None);
}

#[test]
fn the_vr_layout_moves_the_controllers_b_to_the_flashlight() {
    assert_eq!(VR_BUTTONS, [Remap { from: button::B, to: button::DPAD_DOWN }]);
    assert_eq!(binding_dl2(Action::Flashlight), Binding::Pad(buttons(VR_BUTTONS[0].to)), "B reaches the flashlight");
    assert_eq!(binding_dl2(Action::Crouch), Binding::Toggle(buttons(VR_BUTTONS[0].from)), "the duck taps the B the controllers' B left");
}

#[test]
fn the_vr_layout_applies_in_play_but_not_in_a_menu_or_a_wheel() {
    assert!(playing(false, None), "no read yet: in play");
    assert!(playing(false, Some(button::A | button::DPAD_DOWN)));
    assert!(!playing(true, Some(0)), "a menu: B goes back");
    assert!(!playing(false, Some(button::DPAD_LEFT)), "the tools' wheel: B picks");
    assert!(!playing(false, Some(button::DPAD_RIGHT | button::B)), "the weapons' wheel");
}

/// The Chrome games' reload by hand before the ready-made, after each gun update: the
/// player's ready gun, then the gesture read from both palms.
fn old_update(gesture: &mut TouchAway, time: f64, players: bool, state: Option<i32>, apart: Option<f32>) -> bool {
    players && state == Some(READY) && apart.is_some_and(|apart| gesture.feed(time, apart, &TouchAwayRules::RELOAD))
}

#[test]
fn a_reload_starts_at_the_gun_updates_the_old_code_started_one() {
    let (mut old, mut new) = (TouchAway::new(), TouchAway::new());
    let asked = Cell::new(false);
    let mut starts_seen = 0;
    for i in 0..3000 {
        let time = i as f64 / 60.0;
        // The left palm to the gun and away about each 1.3 s; the gun firing (not ready) now
        // and then, sometimes through a touch; another character's gun updating too; the
        // palms not fresh for a while.
        let phase = i % 80;
        let apart = match phase {
            _ if (1000..1050).contains(&i) => None,
            0..=19 => Some(0.4 - 0.015 * phase as f32),
            20..=29 => Some(0.08),
            _ => Some((0.08 + 0.02 * (phase - 29) as f32).min(0.5)),
        };
        let state = if (i / 37) % 5 == 4 { Some(2) } else { Some(READY) };
        for players in [false, true] {
            let was = old_update(&mut old, time, players, state, apart);
            // The ready-made at the moment: a gun in the aiming hand, the reload pressed
            // through the game's own way (kept as asked) and taken here.
            let now = starts(
                players,
                || state,
                || {
                    let done = gestured(&mut new, time, Kind::Gun, apart, &Reloading::FRESH.touch);
                    asked.set(asked.get() || done);
                    done
                },
                || asked.replace(false),
            );
            assert_eq!(now, was, "update {i}, the player's: {players}");
            starts_seen += now as usize;
        }
    }
    assert_eq!(old, new, "the gestures end in the same state");
    assert!(starts_seen >= 10, "{starts_seen}");
}
