//! The player camera update (the one camera call the eye cameras replace), and the PC check that
//! writing it reaches the picture (`turn_yaw`): the update's forward and up are turned about the
//! world's up axis before the renderer stores them, so the picture turns while the character keeps
//! its look. Nothing is kept: the game sets the camera again every frame, so the next frame after
//! the hook is gone is the game's own.

use monaka_hook::{AtomicF32, mem};
use monaka_producer::log;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

/// The player camera update's return address (absolute; 0 until known), and whether and how far
/// (radians) the turn is on.
static PLAYER_UPDATE: AtomicUsize = AtomicUsize::new(0);
static TURNING: AtomicBool = AtomicBool::new(false);
static YAW: AtomicF32 = AtomicF32::zero();
static TURNED: AtomicU64 = AtomicU64::new(0);
static LAST_OUT: [AtomicF32; 3] = [const { AtomicF32::zero() }; 3];
static FED_BACK: AtomicBool = AtomicBool::new(false);

/// A vector as the engine reads it, on a 16-byte boundary (the engine's matrix code reads with
/// aligned loads).
#[repr(C, align(16))]
pub struct Vec3(pub [f32; 4]);

impl Vec3 {
    pub fn address(&self) -> usize {
        self.0.as_ptr() as usize
    }
}

/// The player camera update's return address (checked by the caller).
pub fn set_player_update(address: usize) {
    PLAYER_UPDATE.store(address, Release);
}

/// Whether a camera call came from the player camera update.
pub fn is_player_update(caller: usize) -> bool {
    let known = PLAYER_UPDATE.load(Acquire);
    known != 0 && caller == known
}

pub fn enable_turn(yaw_degrees: f32) {
    YAW.store(yaw_degrees.to_radians());
    TURNING.store(true, Release);
    log!("turning the player camera by {yaw_degrees} degrees about y");
}

pub fn disable() {
    if !TURNING.swap(false, Release) {
        return;
    }
    log!("player camera turned in {} updates{}", TURNED.load(Relaxed), if FED_BACK.load(Relaxed) { " (stopped: fed back)" } else { "" });
}

/// `v` turned by `yaw` about +y.
pub fn turn_about_y(v: [f32; 3], yaw: f32) -> [f32; 3] {
    let (s, c) = yaw.sin_cos();
    [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c]
}

/// For the player camera update (`caller`), the turned forward and up to pass on instead of the
/// game's; `None` leaves the call alone. Stops for good if the game ever hands back the forward it
/// was given (its next frame built on ours would turn again every frame).
pub fn turned(caller: usize, forward: usize, up: usize) -> Option<(Vec3, Vec3)> {
    if !TURNING.load(Acquire) || !is_player_update(caller) || FED_BACK.load(Relaxed) {
        return None;
    }
    let f = mem::read::<[f32; 3]>(forward)?;
    let u = mem::read::<[f32; 3]>(up)?;
    if !f.iter().chain(&u).all(|x| x.is_finite()) {
        return None;
    }
    let last = [LAST_OUT[0].load(), LAST_OUT[1].load(), LAST_OUT[2].load()];
    if TURNED.load(Relaxed) > 0 && f.iter().zip(&last).all(|(a, b)| (a - b).abs() < 1e-6) {
        FED_BACK.store(true, Relaxed);
        log!("the game handed back the turned forward; turning stops");
        return None;
    }
    let yaw = YAW.load();
    let (f, u) = (turn_about_y(f, yaw), turn_about_y(u, yaw));
    for (slot, value) in LAST_OUT.iter().zip(f) {
        slot.store(value);
    }
    TURNED.fetch_add(1, Relaxed);
    Some((Vec3([f[0], f[1], f[2], 0.0]), Vec3([u[0], u[1], u[2], 0.0])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_about_y_keeps_height_and_length() {
        let v = turn_about_y([1.0, 0.5, 0.0], 90f32.to_radians());
        assert!((v[0]).abs() < 1e-6 && (v[1] - 0.5).abs() < 1e-6 && (v[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn vectors_are_aligned() {
        assert_eq!(std::mem::align_of::<Vec3>(), 16);
    }
}
