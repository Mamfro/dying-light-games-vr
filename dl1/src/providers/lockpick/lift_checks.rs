//! Dying Light 1's lockpicking by hand on the ready-made against its own before.

use super::*;

/// What the minigame's read of `action` answered before for a hand twisted `turn`: the pick's
/// direction at the turn times its gain, at most a quarter turn either way; the screwdriver's left
/// or right, the turn over its full turn.
fn before(action: u32, turn: f32) -> f32 {
    let [up, down, left, right, driver_left, _] = READS_OF;
    if [up, down, left, right].contains(&action) {
        let angle = (turn * 1.0).clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
        let (cos, sin) = (angle.cos(), angle.sin());
        match action {
            a if a == up => cos.max(0.0),
            a if a == down => (-cos).max(0.0),
            a if a == right => sin.max(0.0),
            _ => (-sin).max(0.0),
        }
    } else {
        let amount = (turn / 0.8).clamp(-1.0, 1.0);
        if action == driver_left { (-amount).max(0.0) } else { amount.max(0.0) }
    }
}

#[test]
fn each_read_is_answered_as_before() {
    for action in READS_OF {
        let followed = followed(action).unwrap();
        for turn in [-3.0f32, -1.2, -0.8, -0.1, 0.0, 0.3, 0.9, 2.5] {
            assert_eq!(answer(action, Dl1::LOCKPICKING.answer(followed, turn)), before(action, turn), "{action:#x} {turn}");
        }
    }
}
