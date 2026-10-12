//! The Chrome games' lockpicking rules on the ready-made against their own before.

use super::*;
use monaka_core::math::wrap_degrees;

#[test]
fn the_pick_and_the_turn_are_the_ones_worked_out_before() {
    for (now, began) in [(0.0f32, 0.0f32), (30.0, 0.0), (-75.0, 10.0), (170.0, -170.0), (-179.0, 179.0), (89.9, -0.1)] {
        // Before: the wrist's twist since the lock began (degrees), the pick twice it in radians,
        // the turn over 60 degrees.
        let pick = (wrap_degrees(now - began) * 2.0).to_radians();
        let turn = (wrap_degrees(now - began) / 60.0).clamp(-1.0, 1.0);
        let twist = wrap_degrees(now - began).to_radians();
        assert_eq!(LOCKPICKING.answer(Action::Pick, twist), pick, "{now} {began}");
        assert!((LOCKPICKING.answer(Action::Turn, twist) - turn).abs() < 1e-6, "{now} {began}");
        // The poll answers the stick's or the turn's, whichever is more.
        for stick in [0.0f32, 0.3, 1.0] {
            assert!((stick.max(LOCKPICKING.answer(Action::Turn, twist).abs()) - stick.max(turn.abs())).abs() < 1e-6);
        }
    }
}
