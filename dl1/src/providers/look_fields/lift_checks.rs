//! DL1's look targets in Monaka's target units against the conversions DL1 had: the yaw nearest
//! the character's own, the pitch scaled by the convention and clamped to its limit.

use super::*;
use monaka_core::math::wrap_degrees;

/// DL1's target yaw before, for a look `look` degrees in the world (positive left).
fn old_yaw(current: f32, look: f32) -> f32 {
    current + wrap_degrees(-90.0 - look - current)
}

/// DL1's target pitch before.
fn old_pitch(pitch: f32) -> f32 {
    let limit = FIELDS.convention.pitch_limit;
    (pitch * FIELDS.convention.pitch_per_radian).clamp(-limit, limit)
}

#[test]
fn the_targets_are_the_ones_written_before() {
    let units = UNITS;
    for current in [-725.0f32, -180.0, -90.0, 0.0, 12.5, 179.0, 540.0, 1e-3, -33.3] {
        for k in -400..=400 {
            // Head aim's look: the facing plus the head, wrapped (radians), in degrees before.
            let look = monaka_core::math::wrap_radians(k as f32 * 0.0079);
            assert_eq!(units.yaw(current, look).to_bits(), old_yaw(current, look.to_degrees()).to_bits(), "{current} {look}");
        }
    }
    for k in -300..=300 {
        let pitch = k as f32 * 0.0071;
        assert_eq!(units.pitch(pitch).to_bits(), old_pitch(pitch).to_bits(), "{pitch}");
    }
}
