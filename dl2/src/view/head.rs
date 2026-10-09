//! The head pose: from the viewer's pose channel, or synthetic on PC runs (`HEAD`).

use crate::config::{self, debug};
use monaka_channel::pose::{HeadSource, PoseChannel, Synthetic};
use monaka_core::protocol::HeadPose;

pub static HEAD: HeadSource = HeadSource::new();

/// Opens the head: the viewer's pose channel, and the run's synthetic head if it has one.
pub fn open(channel: Option<PoseChannel>) {
    let synthetic = config::get().synthetic.map(|(yaw, pitch)| Box::new(move || synthetic(yaw, pitch)) as Synthetic);
    HEAD.open(channel, synthetic);
}

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
