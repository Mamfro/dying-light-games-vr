//! Provider: same-frame stereo's engine side (`stereo=same`, [`crate::SAME_FRAME`]): both eyes
//! rendered in each game frame, the right eye by a second scene call at the frame's present
//! request, after the left eye has presented (as Dying Light 2 does, on this build's counterparts
//! of its engine functions).
//!
//! The Beast renders one game frame behind its updates: the player camera update that runs during
//! a scene sets the camera for the next frame, which the engine copies into the level's camera
//! (level + 0x13a0) just before that frame's scene. So each update writes a pair's left eye
//! ([`crate::providers::eye_site`]), and the frame whose level camera holds it renders the pair:
//!
//! 1. the frame's present request presents the left eye (the present loop labels and keeps it);
//! 2. under the renderer's render lock, the level's "prepared this frame" stamps undone, the
//!    level camera set to the waiting right eye (its matrix and, with the headset's field of view,
//!    its frustum), the scene called again: the right eye's scene skips the work that does not
//!    depend on the eye ([`once`]);
//! 3. the right eye presented (labelled by its camera as the left was), the level camera given
//!    its left eye back, the stamps undone again and the lock let go.
//!
//! The player camera, which the game thread writes during the scenes, is never touched here.
//!
//! Each eye's scene renders against that eye's own previous frame while the upscaler per eye runs
//! ([`eye_history`]).

use crate::Beast;
use crate::providers::engine::{self, EnterFn, FramePresentFn, LeaveFn, LevelFn, PrepareFn, SceneFn, SetMatrixFn};
use crate::providers::eye_history::{self, camera_position as position};
use crate::providers::{cameras, eye_site};
use monaka_core::camera::Mat34;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use monaka_stereo::feature::DRIVER;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

mod once;

static SCENE_ORIGINAL: Original<SceneFn> = Original::new();
static PREPARE_ORIGINAL: Original<PrepareFn> = Original::new();
static FRAME_PRESENT_ORIGINAL: Original<FramePresentFn> = Original::new();

/// The engine functions the pair calls, checked at the install.
struct Calls {
    engine: usize,
    reset_level: LevelFn,
    enter: EnterFn,
    leave: LeaveFn,
    set_matrix: SetMatrixFn,
}

static CALLS: std::sync::OnceLock<Calls> = std::sync::OnceLock::new();

/// The frame's scene call, as the frame's own task made it (repeated for the right eye).
#[derive(Clone, Copy)]
struct SceneCall {
    game: usize,
    token: u32,
    a: usize,
    b: usize,
}

static LAST_SCENE: Mutex<Option<SceneCall>> = Mutex::new(None);
/// The level camera and the viewport the last prepare filled the frame's data with.
static LEVEL_CAMERA: AtomicUsize = AtomicUsize::new(0);
static VIEWPORT: AtomicUsize = AtomicUsize::new(0);
/// Experiment (`same_frame_hold_parity=1`): the viewport's frame number, which every render
/// advances (and with it the render script's frame parity), held back for the right eye's render so
/// both eyes of a pair render as one frame.
static HOLD_PARITY: AtomicBool = AtomicBool::new(false);
/// The viewport's frame number (u32): `SViewport::OnRender` adds one per render.
const VIEWPORT_FRAME: usize = 0x74;
/// The right eye's scene call is running (its eye-independent work is skipped; [`once`]).
pub(super) static RIGHT_SCENE: AtomicBool = AtomicBool::new(false);

/// Pairs rendered, and the frames that rendered one eye only (by why).
static PAIRS: AtomicU64 = AtomicU64::new(0);
static NO_RIGHT_EYE: AtomicU64 = AtomicU64::new(0);
static NOT_RUNNING: AtomicU64 = AtomicU64::new(0);

/// The same-frame component's install: the scene, prepare and the frame's present request hooked,
/// the engine functions the pair calls checked, the right eye's skips hooked.
pub(crate) fn install(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let (engine_module, renderer) = (&start.build.engine, &start.build.renderer);
    let calls = checked_calls(engine_module, renderer)?;
    let _ = CALLS.set(calls);
    start.hooks.site(&SCENE_ORIGINAL, engine_module, &engine::SCENE, scene as SceneFn)?;
    start.hooks.site(&PREPARE_ORIGINAL, engine_module, &engine::PREPARE, prepare as PrepareFn)?;
    // SAFETY: the shim stub takes the hooked function's three arguments and passes the caller.
    let detour = unsafe { monaka_hook::probe::as_detour::<FramePresentFn>(frame_present_stub) };
    start.hooks.site(&FRAME_PRESENT_ORIGINAL, renderer, &engine::FRAME_PRESENT, detour)?;
    once::install(start.hooks, renderer, start.options.switch("same_frame_skips", true))?;
    HOLD_PARITY.store(start.options.switch("same_frame_hold_parity", false), Relaxed);
    eye_history::arm(eye_history::wanted(start.options));
    log!("same-frame stereo: each frame's right eye rendered by a second scene call at its present request");
    Ok(())
}

/// The engine and renderer functions the pair calls, each checked by its first bytes.
fn checked_calls(engine_module: &Module, renderer: &Module) -> Result<Calls, Rejection> {
    let function = |module: &Module, (rva, bytes): (usize, &[u8]), name: &str| -> Result<usize, Rejection> {
        if module.bytes_match(rva, bytes) { Ok(module.at(rva)) } else { Err(Rejection::revision(format!("signature mismatch: {name}"))) }
    };
    let reset = function(engine_module, engine::RESET_LEVEL, "level reset")?;
    let enter = function(renderer, engine::RENDERER_ENTER, "render lock in")?;
    let leave = function(renderer, engine::RENDERER_LEAVE, "render lock out")?;
    let set_matrix = renderer
        .export("?SetView@CCamera@@UEAAXAEBVmtx34@@_N@Z")
        .ok_or_else(|| Rejection::revision("the renderer does not export CCamera::SetView(mtx34)"))?;
    // SAFETY: each address is the start of a function of that type in this build (checked above,
    // or found by its exported name).
    unsafe {
        Ok(Calls {
            engine: engine_module.base(),
            reset_level: std::mem::transmute::<usize, LevelFn>(reset),
            enter: std::mem::transmute::<usize, EnterFn>(enter),
            leave: std::mem::transmute::<usize, LeaveFn>(leave),
            set_matrix: std::mem::transmute::<usize, SetMatrixFn>(set_matrix),
        })
    }
}

unsafe extern "system" fn scene(game: usize, token: u32, a: usize, b: usize) -> usize {
    let _flight = InFlight::enter();
    let right = RIGHT_SCENE.load(Acquire);
    if !right {
        *LAST_SCENE.locked() = Some(SceneCall { game, token, a, b });
    }
    // The frame's own scene is the pair's left eye while stereo runs, the second call its right.
    let swap = eye_history::before_scene(VIEWPORT.load(Acquire), usize::from(right));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SCENE_ORIGINAL.get()(game, token, a, b) };
    eye_history::after_scene(swap);
    result
}

unsafe extern "system" fn prepare(game: usize, data: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { PREPARE_ORIGINAL.get()(game, data) };
    if !RIGHT_SCENE.load(Acquire)
        && let Some(camera) = mem::read::<usize>(data + engine::FRAME_MAIN_VIEW).filter(|&c| c != 0)
    {
        LEVEL_CAMERA.store(camera, Release);
        VIEWPORT.store(mem::read::<usize>(data + engine::DATA_VIEWPORT).unwrap_or(0), Release);
    }
    result
}

/// A camera's matrix, projection and frustum block, to give back after the right eye.
#[derive(Clone, Copy)]
struct Saved {
    matrix: Mat34,
    projection: [f32; 16],
    frustum: [f32; engine::CAMERA_FRUSTUM_FLOATS],
}

fn save(camera: usize) -> Option<Saved> {
    Some(Saved {
        matrix: mem::read(camera + engine::CAMERA_INVERSE)?,
        projection: mem::read(camera + engine::CAMERA_PROJECTION)?,
        frustum: mem::read(camera + engine::CAMERA_EXTENTS)?,
    })
}

/// Sets `camera`'s camera-to-world matrix through the renderer's own setter.
fn set_matrix(calls: &Calls, camera: usize, matrix: &Mat34) {
    let aligned = monaka_core::Aligned16(*matrix);
    // SAFETY: the renderer's `CCamera::SetView(mtx34, bool)` on the level's live camera, with a
    // 16-byte aligned 3x4.
    unsafe { (calls.set_matrix)(camera, aligned.0.as_ptr() as usize, 0) };
}

/// Gives `camera` back what [`save`] took: its frustum, projection and matrix.
fn restore(calls: &Calls, camera: usize, saved: &Saved) {
    mem::write(camera + engine::CAMERA_EXTENTS, saved.frustum);
    mem::write(camera + engine::CAMERA_PROJECTION, saved.projection);
    set_matrix(calls, camera, &saved.matrix);
}

/// Points `camera` at the right eye: its matrix and, with the headset's field of view, its frustum.
fn aim_right(calls: &Calls, camera: usize, right: &Mat34, pose: &monaka_core::protocol::HeadPose) {
    let near = mem::read::<f32>(camera + engine::CAMERA_NEAR).unwrap_or(f32::NAN);
    if let Some(extents) = eye_site::eye_extents(pose, 1, near) {
        mem::write(camera + engine::CAMERA_EXTENTS, extents);
        // SAFETY: the renderer's projection rebuild (the original, past the eye site's hook) on the
        // level's live camera.
        unsafe { cameras::COMPUTE_FRUSTUM_ORIGINAL.get()(camera, 0) };
    }
    set_matrix(calls, camera, right);
}

monaka_hook::caller_shim!(frame_present_stub => frame_present);

/// The frame's present request: the left eye presents, then the frame's right eye is rendered and
/// presented (when its left eye is the one this frame rendered).
unsafe extern "system" fn frame_present(renderer: usize, a: usize, b: usize, caller: usize) -> usize {
    let _flight = InFlight::enter();
    // SAFETY (each call of `original`): the original, with the game's arguments.
    let original = FRAME_PRESENT_ORIGINAL.get();
    let Some(calls) = CALLS.get() else { return unsafe { original(renderer, a, b) } };
    if caller != calls.engine + engine::NORMAL_PRESENT_CALLER || RIGHT_SCENE.load(Acquire) {
        return unsafe { original(renderer, a, b) };
    }
    if !eye_site::running() {
        NOT_RUNNING.fetch_add(1, Relaxed);
        return unsafe { original(renderer, a, b) };
    }
    let camera = LEVEL_CAMERA.load(Acquire);
    let scene_call = *LAST_SCENE.locked();
    let pair = (camera != 0).then(|| position(camera)).flatten().and_then(eye_site::right_eye_for);
    // The left eye.
    let result = unsafe { original(renderer, a, b) };
    let (Some((right, pose)), Some(scene_call), Some(saved)) = (pair, scene_call, save(camera)) else {
        let n = NO_RIGHT_EYE.fetch_add(1, Relaxed);
        if monaka_producer::log::seldom(n, 4) {
            log!("same-frame: a frame without its right eye ({} so far): {}", n + 1, if pair.is_none() { "no right eye waits for its left eye" } else { "no scene call or camera" });
        }
        return result;
    };
    // The right eye's choice is taken in turn, as the updates take theirs: one per present.
    let _ = DRIVER.take_in_turn();
    DRIVER.camera_written([right[3], right[7], right[11]], 1, pose);
    let level = camera - engine::LEVEL_CAMERA;
    // SAFETY: the renderer's render lock on the live renderer, the level reset on the live level.
    unsafe {
        (calls.enter)(renderer, std::ptr::null());
        (calls.reset_level)(level);
    }
    aim_right(calls, camera, &right, &pose);
    let viewport = VIEWPORT.load(Acquire);
    let frame_before = (viewport != 0).then(|| mem::read::<u32>(viewport + VIEWPORT_FRAME)).flatten();
    if HOLD_PARITY.load(Relaxed)
        && let Some(n) = frame_before
    {
        mem::write::<u32>(viewport + VIEWPORT_FRAME, n.wrapping_sub(1));
    }
    RIGHT_SCENE.store(true, Release);
    // SAFETY: the scene detour (each eye's own history), with this frame's own arguments.
    unsafe { scene(scene_call.game, scene_call.token, scene_call.a, scene_call.b) };
    let n = PAIRS.load(Relaxed);
    if n < 6 {
        let frame_after = (viewport != 0).then(|| mem::read::<u32>(viewport + VIEWPORT_FRAME)).flatten();
        log!("same-frame: pair {n}: the viewport's frame number {frame_before:?} after the left eye, {frame_after:?} after the right{}", if HOLD_PARITY.load(Relaxed) { " (held)" } else { "" });
    }
    // The right eye.
    unsafe { original(renderer, a, b) };
    RIGHT_SCENE.store(false, Release);
    restore(calls, camera, &saved);
    // SAFETY: as above.
    unsafe {
        (calls.reset_level)(level);
        (calls.leave)(renderer);
    }
    let n = PAIRS.fetch_add(1, Relaxed);
    if n == 0 {
        log!("same-frame: first pair rendered (thread {})", monaka_hook::thread_id());
    }
    result
}

/// The pairs' counts.
pub fn report() {
    log!(
        "same-frame: {} pairs rendered, {} frames without their right eye, {} frames while stereo did not run; {}; {}",
        PAIRS.load(Relaxed),
        NO_RIGHT_EYE.load(Relaxed),
        NOT_RUNNING.load(Relaxed),
        once::report(),
        eye_history::summary()
    );
}
