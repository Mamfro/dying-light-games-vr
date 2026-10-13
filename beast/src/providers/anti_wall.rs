//! Provider, to be: settings (a game setting changed and put back): the first-person arms drawn
//! where they are while VR runs with the hand rig.
//!
//! The Beast draws the first-person arms scaled toward the camera so they never reach into a wall:
//! its anti-wall scales (`AntiWallHandsScale` 0.34, `AntiWallHangHandsScale` 0.2,
//! `AntiWallAimHandsScale` 0.5, `AntiWallLowCrawlScale` 0.3, player parameters; the camera effect
//! `f_fpp_scale`). On a flat screen that looks the same; in the depth buffer the arms are then a
//! third of their distance away. Frame warping moves each pixel by its depth, so the arms landed
//! far beside where the other eye sees them (a ghost of the hand in the warped eye), and the
//! headset's own reprojection had them as near. While VR runs the scales are held at 1 (as Dying
//! Light 1's anti-wall squash is turned off), held at every look update ([`hold`]) as the game may
//! load its parameters again, and the game's own put back when VR stops ([`give_back`]).

use crate::providers::engine::{ANTI_WALL_SCALES, PARAMETERS_SLOT};
use monaka_core::sync::Locked;
use monaka_hook::mem;
use monaka_producer::log;
use std::sync::Mutex;

/// The parameter block held, and the game's scales in it before.
static SAVED: Mutex<Option<(usize, [f32; 4])>> = Mutex::new(None);

/// One look update of `player` (its class's vtable `vtable`): the scales held at 1 while `on` (VR
/// running), the game's put back once it is not.
pub fn hold(player: usize, vtable: usize, on: bool) {
    if !on {
        give_back();
        return;
    }
    let Some(getter) = mem::read::<usize>(vtable + PARAMETERS_SLOT).filter(|&g| g != 0) else { return };
    // SAFETY: the player's own parameter getter, no arguments beyond it, on the game thread, as the
    // game's own controllers call it.
    let block = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(player) };
    if block == 0 {
        return;
    }
    let mut saved = SAVED.locked();
    if saved.is_none_or(|(held, _)| held != block) {
        let Some(scales) = read(block) else { return };
        *saved = Some((block, scales));
        log!("arms: the game's anti-wall scales held at 1 while VR runs (the game's {scales:?})");
    }
    for at in ANTI_WALL_SCALES {
        mem::write(block + at, 1.0f32);
    }
}

/// The game's own scales back (VR stopped, or the hooks going out).
pub fn give_back() {
    if let Some((block, scales)) = SAVED.locked().take() {
        for (at, scale) in ANTI_WALL_SCALES.into_iter().zip(scales) {
            mem::write(block + at, scale);
        }
        log!("arms: the game's anti-wall scales given back");
    }
}

/// The scales in `block`, when they read as scales.
fn read(block: usize) -> Option<[f32; 4]> {
    let mut scales = [0.0; 4];
    for (scale, at) in scales.iter_mut().zip(ANTI_WALL_SCALES) {
        *scale = mem::read::<f32>(block + at).filter(|s| s.is_finite() && *s > 0.0 && *s <= 1.0)?;
    }
    Some(scales)
}
