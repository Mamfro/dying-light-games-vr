//! Head aim's probes (`monaka_arms::owned_facing`, on `crate::providers::look_fields`).
//!
//! - `probe_look` (with head aim): how head aim steers the character.
//! - `probe_cutscene`: each look update while the cutscene trace runs ([`super::view`]).

use super::options;
use monaka_arms::owned_facing::{Steered, Traced};
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// The latest update's stored and written target yaw (degrees, as f32 bits), for `probe_frames`.
static LAST_YAWS: AtomicU64 = AtomicU64::new(0);

/// Head aim steered the character.
pub fn steered(s: &Steered) {
    LAST_YAWS.store(((s.yaw.to_bits() as u64) << 32) | s.target.to_bits() as u64, Relaxed);
    if options().look && (s.frame <= 6 || s.frame.is_multiple_of(120)) {
        log!(
            "head aim {}: camera yaw {:.2} (positive left), facing {:.2}, aim yaw {:.2} pitch {:.2}; target yaw {:.2} -> {:.2}, pitch {:.2} -> {:.2}; current pitch {:.2}",
            s.frame,
            s.camera_yaw,
            s.facing,
            s.aim_yaw,
            s.aim_pitch,
            s.yaw,
            s.target,
            s.pitch,
            s.target_pitch,
            mem::read::<f32>(s.character + crate::providers::engine::CURRENT_PITCH).unwrap_or(f32::NAN)
        );
    }
}

/// The latest update's target yaw as the game had left it and as head aim wrote it.
pub fn last_yaws() -> (f32, f32) {
    let bits = LAST_YAWS.load(Relaxed);
    (f32::from_bits((bits >> 32) as u32), f32::from_bits(bits as u32))
}

/// `probe_cutscene`: one look update (degrees; `facing` none before head aim knows it).
pub fn traced(update: &Traced) {
    let Traced { frame, yaw, pitch, camera_yaw, facing, head_yaw, cutscene: movie } = *update;
    if options().cutscene && super::view::cutscene_line() {
        log!("cs look update {frame}: yaw read {yaw:.2} pitch read {pitch:.2} camera yaw {camera_yaw:.2} facing {facing:?} head {head_yaw:.2} movie {movie}");
    }
}
