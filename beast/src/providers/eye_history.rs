//! Provider: each eye's own previous frame in the renderer, for the upscaler per eye
//! ([`crate::EYE_HISTORY`] with alternate eyes; [`crate::providers::same_frame`] calls
//! [`before_scene`] and [`after_scene`] around its own scene calls).
//!
//! The viewport's block of last frame's camera ([`HISTORY`]), which a scene writes after it submits
//! and the next render reads (the upscaler's constants and motion vectors among it), is each eye's
//! own: put in place before that eye's scene and kept after it. The game then describes each eye's
//! motion against that eye's own previous frame, as in play without VR, and the upscaler per eye
//! passes it as it is (`use_game_history`, DLSS's and FSR's). Only while the upscaler per eye runs:
//! one history for both eyes needs the game's own previous frame, the other eye's.
//!
//! With alternate eyes an eye's previous frame is two game frames back: the camera's motion is
//! measured over both, objects' own motion as the game keeps it. The eye a scene renders is the one
//! whose camera the level camera holds (`Driver::eye_at`: the level camera is the player camera's
//! copy, written by the update before).

use crate::Beast;
use crate::providers::engine::{self, PrepareFn, SceneFn};
use crate::providers::eye_site;
use monaka_core::options::Options;
use monaka_core::sync::Locked;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::feature::DRIVER;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

/// The viewport's last frame camera (view, inverse, view-projection, translated view-projection,
/// projection, position, forward): offset and size.
const HISTORY: (usize, usize) = (0x80, 0x138);
/// The last frame camera's position in the viewport.
const HISTORY_POSITION: usize = 0x1a0;
/// Each eye keeps its own last frame camera (`eye_history`, with the upscaler per eye).
static ON: AtomicBool = AtomicBool::new(false);
static KEPT: Mutex<[Option<[u8; HISTORY.1]>; 2]> = Mutex::new([None; 2]);
/// Scenes rendered with their eye's own last frame, and scenes whose eye was not known.
static SWAPPED: AtomicU64 = AtomicU64::new(0);
static UNKNOWN: AtomicU64 = AtomicU64::new(0);

static SCENE_ORIGINAL: Original<SceneFn> = Original::new();
static PREPARE_ORIGINAL: Original<PrepareFn> = Original::new();
/// The level camera and the viewport the last prepare filled the frame's data with.
static LEVEL_CAMERA: AtomicUsize = AtomicUsize::new(0);
static VIEWPORT: AtomicUsize = AtomicUsize::new(0);

/// Whether each eye keeps its own last frame camera (`eye_history`, on by default).
pub(crate) fn wanted(options: &Options) -> bool {
    options.switch("eye_history", true)
}

/// Each eye's own last frame kept from now on (`on`), none kept yet.
pub(crate) fn arm(on: bool) {
    ON.store(on, Relaxed);
    *KEPT.locked() = [None; 2];
}

/// A scene with its eye's own last frame in place ([`before_scene`]), kept after it.
pub(crate) struct Swap {
    viewport: usize,
    eye: usize,
}

/// Before `eye`'s scene renders with `viewport`: that eye's own last frame put in place, while the
/// upscaler per eye runs.
pub(crate) fn before_scene(viewport: usize, eye: usize) -> Option<Swap> {
    let active = ON.load(Relaxed) && viewport != 0 && eye < 2 && eye_site::running() && monaka_stereo::upscaler::running();
    let mut kept = KEPT.locked();
    if !active {
        *kept = [None; 2];
        return None;
    }
    if let Some(own) = kept[eye] {
        let n = SWAPPED.fetch_add(1, Relaxed);
        if (2..6).contains(&n) {
            let at = |block: &[u8; HISTORY.1]| -> [f32; 3] {
                let start = HISTORY_POSITION - HISTORY.0;
                std::array::from_fn(|i| f32::from_le_bytes(block[start + 4 * i..start + 4 * i + 4].try_into().unwrap_or_default()))
            };
            let game = mem::read::<[f32; 3]>(viewport + HISTORY_POSITION);
            log!("eye history: eye {eye} renders with its own last frame at {:?} in place of {game:?}", at(&own));
        }
        mem::write(viewport + HISTORY.0, own);
    }
    Some(Swap { viewport, eye })
}

/// After the scene [`before_scene`] readied: its last frame camera kept for its eye's next.
pub(crate) fn after_scene(swap: Option<Swap>) {
    if let Some(Swap { viewport, eye }) = swap {
        KEPT.locked()[eye] = mem::read::<[u8; HISTORY.1]>(viewport + HISTORY.0);
    }
}

/// A camera's position (its camera-to-world translation).
pub(crate) fn camera_position(camera: usize) -> Option<[f32; 3]> {
    let m = mem::read::<[f32; 12]>(camera + engine::CAMERA_INVERSE)?;
    let at = [m[3], m[7], m[11]];
    at.iter().all(|v| v.is_finite()).then_some(at)
}

/// The eye history's install with alternate eyes ([`crate::EYE_HISTORY`]): the scene and prepare
/// hooked.
pub(crate) fn install(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let engine_module = &start.build.engine;
    start.hooks.site(&SCENE_ORIGINAL, engine_module, &engine::SCENE, scene as SceneFn)?;
    start.hooks.site(&PREPARE_ORIGINAL, engine_module, &engine::PREPARE, prepare as PrepareFn)?;
    arm(true);
    log!("eye history: each eye's scene renders against its own last frame while the upscaler per eye runs");
    Ok(())
}

unsafe extern "system" fn scene(game: usize, token: u32, a: usize, b: usize) -> usize {
    let _flight = InFlight::enter();
    let camera = LEVEL_CAMERA.load(Acquire);
    let eye = (camera != 0).then(|| camera_position(camera)).flatten().and_then(|at| DRIVER.eye_at(at));
    if eye.is_none() && eye_site::running() {
        UNKNOWN.fetch_add(1, Relaxed);
    }
    let swap = eye.and_then(|eye| before_scene(VIEWPORT.load(Acquire), eye));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SCENE_ORIGINAL.get()(game, token, a, b) };
    after_scene(swap);
    result
}

unsafe extern "system" fn prepare(game: usize, data: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { PREPARE_ORIGINAL.get()(game, data) };
    if let Some(camera) = mem::read::<usize>(data + engine::FRAME_MAIN_VIEW).filter(|&c| c != 0) {
        LEVEL_CAMERA.store(camera, Release);
        VIEWPORT.store(mem::read::<usize>(data + engine::DATA_VIEWPORT).unwrap_or(0), Release);
    }
    result
}

/// The scenes' counts.
pub(crate) fn summary() -> String {
    format!("eye history: {} scenes rendered against their eye's own last frame, {} scenes of no known eye", SWAPPED.load(Relaxed), UNKNOWN.load(Relaxed))
}

pub(crate) fn report() {
    log!("{}", summary());
}
