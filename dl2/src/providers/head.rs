//! Provider: the synthetic head of PC runs (the same-frame loop's `PairLoop::synthetic`; the head
//! source itself is the loop's, `monaka_stereo::same_frame::PAIRS`).

use crate::config::{self, debug};
use monaka_core::protocol::HeadPose;

/// A head turned by `yaw` then pitched by `pitch` (degrees; positive left and up), with
/// headset-like fields of view; the debug flags sweep it and change its field of view.
pub fn synthetic(mut yaw: f32, pitch: f32) -> HeadPose {
    let now = monaka_channel::tick() as f32;
    if config::debug(debug::SWEEP) {
        yaw += 15.0 * (now * 0.0015708).sin();
    }
    if config::debug(debug::FAST_SWEEP) {
        yaw += 40.0 * (now * 0.0062832).sin();
    }
    let mut head = HeadPose::synthetic(yaw, pitch, HeadPose::SYNTHETIC_FOV, 0.064);
    if config::debug(debug::SYMMETRIC_FOV) {
        head.fov = [[-0.8, 0.8, 0.8, -0.8]; 2];
    }
    if config::debug(debug::LOW_FOV) {
        for eye in &mut head.fov {
            eye[2] = 0.70;
            eye[3] = -0.95;
        }
    }
    head
}
