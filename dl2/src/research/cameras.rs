//! `probe_cameras`: what drives the frame's camera (`crate::player::aim`, `crate::view::scene`).
//!
//! Two signs of a cutscene as engine state, for finding one: another caller than the player camera
//! update setting the player camera, and a frame rendered from a camera the player camera update
//! did not write lately (a movie camera).

use super::options;
use monaka_hook::module::Module;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The last few matrices the player camera update wrote.
static RECENT_PLAYER_CAMERAS: Mutex<[[f32; 12]; 4]> = Mutex::new([[0.0; 12]; 4]);
/// Other call sites that set the player camera, with counts.
static OTHER_CALLERS: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());
/// Frames whose camera was one the player camera update wrote lately, and frames whose was not;
/// whether the latest was, and how many switches were logged.
static FRAMES_FROM_PLAYER: AtomicU64 = AtomicU64::new(0);
static FRAMES_FROM_ELSEWHERE: AtomicU64 = AtomicU64::new(0);
static LAST_FROM_PLAYER: AtomicBool = AtomicBool::new(true);
static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// The player camera update wrote the player camera's camera-to-world `matrix`.
pub fn player_camera(matrix: &[f32; 12]) {
    if !options().cameras {
        return;
    }
    if let Ok(mut recent) = RECENT_PLAYER_CAMERAS.lock() {
        recent.rotate_right(1);
        recent[0] = *matrix;
    }
}

/// `FromForwardUpPos` on `camera` called from `site`, not the player camera update.
pub fn other_caller(camera: usize, site: usize) {
    if !options().cameras || camera == 0 || crate::player::aim::AIM.player_camera(camera).is_none() {
        return;
    }
    let Ok(mut callers) = OTHER_CALLERS.lock() else { return };
    if let Some((_, n)) = callers.iter_mut().find(|(s, _)| *s == site) {
        *n += 1;
    } else if callers.len() < 32 {
        log!("camera probe: the player camera set from {} (not the player camera update)", Module::describe(site));
        callers.push((site, 1));
    }
}

/// Whether `inverse` (a camera-to-world the frame renders from) is one the player camera update
/// wrote lately: every entry within 0.02 (metres for the position, cosines for the axes).
fn from_player(inverse: &[f32; 12]) -> bool {
    RECENT_PLAYER_CAMERAS.lock().is_ok_and(|recent| recent.iter().any(|m| m.iter().zip(inverse).all(|(a, b)| (a - b).abs() <= 0.02)))
}

/// The frame's base camera as the game prepared it (not the right eye's repeat of the scene).
pub fn frame(inverse: &[f32; 12], counter: u32) {
    if !options().cameras {
        return;
    }
    let from = from_player(inverse);
    (if from { &FRAMES_FROM_PLAYER } else { &FRAMES_FROM_ELSEWHERE }).fetch_add(1, Ordering::Relaxed);
    if LAST_FROM_PLAYER.swap(from, Ordering::Relaxed) != from && SWITCHES.fetch_add(1, Ordering::Relaxed) < 60 {
        log!(
            "camera probe (counter {counter}): the frame's camera is {} at [{:.2} {:.2} {:.2}]",
            if from { "the player camera update's again" } else { "NOT one the player camera update wrote lately" },
            inverse[3],
            inverse[7],
            inverse[11]
        );
    }
}

/// The end of a run.
pub fn report() {
    if !options().cameras {
        return;
    }
    log!(
        "camera probe: frames from the player camera update {}, from elsewhere {}; other call sites setting the player camera: {}",
        FRAMES_FROM_PLAYER.load(Ordering::Relaxed),
        FRAMES_FROM_ELSEWHERE.load(Ordering::Relaxed),
        OTHER_CALLERS.lock().map(|c| c.iter().map(|(s, n)| format!("{} x{n}", Module::describe(*s))).collect::<Vec<_>>().join(", ")).unwrap_or_default()
    );
}
