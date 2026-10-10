//! The stereo pair on the engine side.
//!
//! Same-frame stereo: the game's own scene call renders the left eye with the tracked camera; when
//! the engine requests its present, the left eye is presented (and copied by the renderer side),
//! then a second scene call on the same simulation frame renders the right eye, which is presented
//! too. Alternate-eye: one eye per frame, taking turns. Depth stereo: one centre render per frame
//! with both eyes' field of view; the renderer side makes the eyes from its depth.
//!
//! Each eye keeps its own temporal history (previous camera, DLSS history), the right eye reuses
//! the left eye's ray-tracing structures (the one costly eye-independent work that is safe to share), and
//! visibility follows the tracked camera instead of the mouse camera.

use crate::view::camera::{self, TrackedEye};
use crate::config::{self, debug};
use crate::engine::{self, *};
use crate::game::{self, read_camera};
use crate::output::temporal::{ALL_CAMERA_WRITERS, HistoryBank, camera_writer_bit};
use crate::{player::aim, view::head, output::render11};
use monaka_core::protocol::HeadPose;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub static SCENE_ORIGINAL: Original<SceneFn> = Original::new();
pub static PREPARE_ORIGINAL: Original<PrepareFn> = Original::new();
pub static SUBMIT_ORIGINAL: Original<SubmitFn> = Original::new();
pub static PRESENT_REQUEST_ORIGINAL: Original<PresentRequestFn> = Original::new();
pub static QUEUE_PHASE_ORIGINAL: Original<QueuePhaseFn> = Original::new();
static VIEW_SETUP_ORIGINAL: Original<ViewSetupFn> = Original::new();
static VISIBILITY_CAMERA_ORIGINAL: Original<VisibilityCameraFn> = Original::new();
static BASE_CAMERA_COPY_ORIGINAL: Original<BaseCameraCopyFn> = Original::new();
static MAIN_VISIBILITY_ORIGINAL: Original<MainVisibilityFn> = Original::new();
static WORLD_VISIBILITY_ORIGINAL: Original<WorldVisibilityFn> = Original::new();
static JUMP_TO_HSM_ORIGINAL: Original<CommandFn> = Original::new();
static HISTORY_LOOKUP_ORIGINAL: Original<HistoryLookupFn> = Original::new();
static EXTERNAL_PASS_ORIGINAL: Original<ExternalPassFn> = Original::new();
static ONCE_ORIGINALS: [Original<OnceFn>; 9] = [const { Original::new() }; 9];

/// Set while the producer publishes; cleared first at stop.
pub static PUBLISHING: AtomicBool = AtomicBool::new(false);
static VR_ACTIVE: AtomicBool = AtomicBool::new(false);
static SAFETY_BLOCKED: AtomicBool = AtomicBool::new(false);
static SUSPEND_UNTIL: AtomicU64 = AtomicU64::new(0);
pub static PAIRS: AtomicU64 = AtomicU64::new(0);
pub static STEREO_PAIRS: AtomicU64 = AtomicU64::new(0);
pub static REJECTED_PAIRS: AtomicU64 = AtomicU64::new(0);
/// When the last stereo pair completed; without a recent one, frames go out mono.
pub static LAST_STEREO_TICK: AtomicU64 = AtomicU64::new(0);
static ALTERNATE_NEXT: AtomicU32 = AtomicU32::new(1);
static ALTERNATE_HEAD: Mutex<Option<HeadPose>> = Mutex::new(None);

pub fn vr_active() -> bool {
    VR_ACTIVE.load(Ordering::Acquire)
}

pub fn publishing() -> bool {
    PUBLISHING.load(Ordering::Acquire)
}

/// What a scene call rendered, and for a left eye what its right eye needs.
#[derive(Clone, Copy, Default)]
pub struct SceneInput {
    game: usize,
    a: usize,
    b: usize,
    camera: usize,
    counter: u32,
    token: u32,
    valid: bool,
    normal_left: bool,
    alternate_eye: u32,
    depth_stereo: bool,
    center_fov: [f32; 4],
    /// The camera the frame was rendered with, and its P00/P11.
    rendered_view: [f32; 12],
    rendered_scale: [f32; 2],
    head: HeadPose,
    eyes: [TrackedEye; 2],
    inverse: [f32; 12],
    projection: [f32; 16],
    frustum: [f32; 12],
    right_inverse: [f32; 12],
    right_projection: [f32; 16],
}

static LAST_SCENE: Mutex<Option<SceneInput>> = Mutex::new(None);

fn last_scene() -> SceneInput {
    LAST_SCENE.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default()
}

fn set_last_scene(input: SceneInput) {
    *LAST_SCENE.lock().unwrap_or_else(|e| e.into_inner()) = Some(input);
}

/// The last scene, with its pending left eye taken (it is restored or paired exactly once).
fn take_left() -> SceneInput {
    let mut last = LAST_SCENE.lock().unwrap_or_else(|e| e.into_inner());
    let input = last.unwrap_or_default();
    if let Some(scene) = last.as_mut() {
        scene.normal_left = false;
    }
    input
}

/// Per render thread: where the scene and the pair are.
#[derive(Default)]
pub struct Local {
    in_scene: Cell<bool>,
    prepared_camera: Cell<usize>,
    stereo_repeat: Cell<bool>,
    tracked_visibility_camera: Cell<usize>,
    visibility_setup_level: Cell<usize>,
    /// True only inside the right eye's extra scene call; eye-independent work is skipped there.
    right_eye_scene: Cell<bool>,
    /// The eye whose present is being dispatched: 1 left, 2 right, 3 depth-stereo centre, 0 none.
    pub dispatch_eye: Cell<u32>,
    /// Eye images the renderer side captured for the current pair.
    pub eye_copies: Cell<u32>,
    pub dispatch_pair: Cell<u64>,
    /// The swapchain whose present the engine is dispatching (D3D11).
    pub dispatched_swap: Cell<usize>,
    /// One head pose for both eyes of a pair.
    pub pair_head: Cell<HeadPose>,
    /// Depth stereo: the centre view's field of view.
    pub pair_fov: Cell<[f32; 4]>,
    /// The game's aim in the rendered view (NDC).
    pub pair_aim: Cell<Option<[f32; 2]>>,
}

thread_local! {
    static LOCAL: Local = Local::default();
}

pub fn local<R>(f: impl FnOnce(&Local) -> R) -> R {
    LOCAL.with(f)
}

// --- Timing ---------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub enum Stage {
    MonoScene,
    LeftScene,
    LeftDrain,
    RightScene,
    RightDrain,
    FrameInterval,
}

const STAGE_NAMES: [&str; 6] = ["mono scene", "left scene", "left drain", "right scene", "right drain", "frame interval"];
static STAGE_NANOS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static STAGE_COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static LAST_FRAME_END: AtomicU64 = AtomicU64::new(0);

fn time(stage: Stage, took: Duration) {
    STAGE_NANOS[stage as usize].fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
    STAGE_COUNTS[stage as usize].fetch_add(1, Ordering::Relaxed);
}

pub fn timing_report() -> String {
    let mut line = String::from("timing (average ms):");
    for (i, name) in STAGE_NAMES.iter().enumerate() {
        let count = STAGE_COUNTS[i].swap(0, Ordering::Relaxed);
        let nanos = STAGE_NANOS[i].swap(0, Ordering::Relaxed);
        if count > 0 {
            line += &format!(" {name} {:.2} (x{count});", nanos as f64 / count as f64 / 1e6);
        }
    }
    line
}

/// A pair finished: frame spacing, the stereo clock and a log line now and then.
pub fn pair_done(kind: &str, counter: u32) {
    let now = START.elapsed().as_nanos() as u64;
    let last = LAST_FRAME_END.swap(now, Ordering::Relaxed);
    if last != 0 {
        time(Stage::FrameInterval, Duration::from_nanos(now.saturating_sub(last)));
    }
    LAST_STEREO_TICK.store(game::tick(), Ordering::Release);
    let n = STEREO_PAIRS.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 3 || n.is_multiple_of(600) {
        log!("{kind} pair {n} counter={counter}");
    }
}

// --- Pausing ----------------------------------------------------------------------------------------
/// A changed or unavailable scene (menus, loading, cut-scenes) pauses stereo for a moment; it
/// resumes by itself.
pub fn suspend(reason: &str) {
    VR_ACTIVE.store(false, Ordering::Release);
    TEMPORAL_RESET.store(true, Ordering::Release);
    ALTERNATE_NEXT.store(1, Ordering::Relaxed);
    SUSPEND_UNTIL.store(game::tick() + 100, Ordering::Release);
    let n = REJECTED_PAIRS.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 8 || n.is_multiple_of(100) {
        log!("stereo paused: {reason} (count {n})");
    }
}

pub fn safety_stop(reason: &str) {
    VR_ACTIVE.store(false, Ordering::Release);
    SAFETY_BLOCKED.store(true, Ordering::Release);
    log!("stereo stopped for this run: {reason}");
}

pub fn stop_stereo() {
    VR_ACTIVE.store(false, Ordering::Release);
}

fn update_vr_active() {
    if !vr_active()
        && !config::debug(debug::MONO)
        && !SAFETY_BLOCKED.load(Ordering::Acquire)
        && game::tick() >= SUSPEND_UNTIL.load(Ordering::Acquire)
        && publishing()
    {
        VR_ACTIVE.store(true, Ordering::Release);
    }
}

// --- Camera state -----------------------------------------------------------------------------------
fn snapshot(game: usize, token: u32, a: usize, b: usize, camera: usize) -> SceneInput {
    let mut input = SceneInput { game, token, a, b, camera, ..SceneInput::default() };
    input.counter = mem::read::<u32>(game + engine::GAME_COUNTER).unwrap_or(0);
    if let Some((inverse, projection, frustum)) = read_camera(camera) {
        (input.inverse, input.projection, input.frustum) = (inverse, projection, frustum);
        input.valid = camera::usable(&inverse);
        // The game's own camera: not the right eye's repeat of the scene.
        if input.valid && !local(|l| l.stereo_repeat.get()) {
            crate::research::cameras::frame(&inverse, input.counter);
        }
    }
    input
}

/// Sets the camera's camera-to-world matrix through the engine's own setter.
///
/// # Safety
/// `camera` must be a live engine camera.
unsafe fn set_camera_matrix(camera: usize, matrix: &[f32; 12]) {
    let aligned = monaka_core::Aligned16(*matrix);
    // SAFETY: the caller's guarantee; the matrix is 16-byte aligned for the setter's loads.
    unsafe { (game::get().component_set)(camera, aligned.0.as_ptr(), false) };
}

fn restore_camera(input: &SceneInput) {
    // SAFETY: `input.camera` was a live engine camera when it was snapshot during this frame; the
    // frustum and projection are plain floats in it, and the component setter takes a 3x4.
    unsafe {
        std::ptr::copy_nonoverlapping(input.frustum.as_ptr(), (input.camera + engine::CAMERA_FRUSTUM) as *mut f32, 12);
        std::ptr::copy_nonoverlapping(input.projection.as_ptr(), (input.camera + engine::CAMERA_PROJECTION) as *mut f32, 16);
        set_camera_matrix(input.camera, &input.inverse);
    }
}

fn apply_eye_frustum(camera: usize, eye: &TrackedEye) {
    // SAFETY: a live engine camera (checked by the caller's snapshot); its frustum is 12 floats.
    unsafe {
        let f = std::slice::from_raw_parts_mut((camera + engine::CAMERA_FRUSTUM) as *mut f32, 12);
        let near = f[4];
        f[0] = near * eye.fov[0].tan();
        f[1] = near * eye.fov[1].tan();
        f[2] = near * eye.fov[3].tan();
        f[3] = near * eye.fov[2].tan();
        // Native projection offsets are applied after the frustum matrix is built.
        f[9] = 0.0;
        f[10] = 0.0;
        f[11] = 0.0;
        (game::get().rebuild_projection)(camera, false);
    }
}

fn camera_matches(input: &SceneInput, inverse: &[f32; 12], projection: &[f32; 16]) -> bool {
    input.inverse.iter().zip(inverse).all(|(a, b)| a.is_finite() && (a - b).abs() < 0.0001)
        && [0, 2, 5, 6].iter().all(|&i| input.projection[i].is_finite() && (input.projection[i] - projection[i]).abs() < 0.001)
}

/// A left eye whose pair never ran still holds the tracked camera: restore it.
pub fn abandon_pair() {
    let input = take_left();
    if input.normal_left {
        restore_camera(&input);
    }
}

// --- Per-eye temporal history ---------------------------------------------------------------------
// Reprojection, TAA, motion blur and DLSS compare each frame with the previous camera. With two
// eyes per frame the native history holds the other eye, so each eye keeps its own here.
static TEMPORAL_SCENE_EYE: AtomicU32 = AtomicU32::new(0);
static TEMPORAL_SCENE_KEY: AtomicU32 = AtomicU32::new(0);
pub static TEMPORAL_EPOCH: AtomicU32 = AtomicU32::new(1);
/// Start each eye's history afresh.
pub static TEMPORAL_RESET: AtomicBool = AtomicBool::new(true);
/// The eye whose frame is being drawn or presented (D3D12 DLSS reads it); 0 outside a pair.
pub static RENDER_EYE: AtomicU32 = AtomicU32::new(0);
static CAMERA_WRITE_TREE: AtomicUsize = AtomicUsize::new(0);
static CAMERA_WRITE_MASK: AtomicU32 = AtomicU32::new(0);
static TEMPORAL_CAMERA: AtomicUsize = AtomicUsize::new(0);
static TEMPORAL_COUNTER: AtomicU32 = AtomicU32::new(0);
static CAMERA_HISTORY: LazyLock<Mutex<HistoryBank<0x140>>> = LazyLock::new(|| Mutex::new(HistoryBank::new(32)));
static EXTERNAL_HISTORY: LazyLock<Mutex<HistoryBank<0x90>>> = LazyLock::new(|| Mutex::new(HistoryBank::new(32)));
static CAMERA_COMMITS: AtomicU64 = AtomicU64::new(0);
static CAMERA_INCOMPLETE: AtomicU64 = AtomicU64::new(0);
static HISTORY_READS: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_SWAPS: AtomicU64 = AtomicU64::new(0);
static SHADOW_JUMPS_SKIPPED: AtomicU64 = AtomicU64::new(0);
static SHADOW_JUMPS_RUN: AtomicU64 = AtomicU64::new(0);
static ONCE_SKIPPED: AtomicU64 = AtomicU64::new(0);
static MENU_FRAMES: AtomicU64 = AtomicU64::new(0);

fn history_eye() -> u32 {
    if config::debug(debug::NATIVE_CAMERA_HISTORY) { 0 } else { TEMPORAL_SCENE_EYE.load(Ordering::Acquire) }
}

monaka_hook::caller_shim!(history_lookup_stub => history_lookup);

unsafe extern "system" fn history_lookup(tree: usize, key: *const u32, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let original = unsafe { HISTORY_LOOKUP_ORIGINAL.get()(tree, key) };
    let eye = history_eye();
    // SAFETY: the game passes a valid key pointer.
    let key = unsafe { *key };
    if original == 0 || !(1..=2).contains(&eye) || key != TEMPORAL_SCENE_KEY.load(Ordering::Acquire) {
        return original;
    }
    let writer = camera_writer_bit(caller.wrapping_sub(game::get().engine));
    if writer != 0 {
        // Native writes stay visible to every consumer; the finished camera is banked after the scene.
        let first = CAMERA_WRITE_TREE.compare_exchange(0, tree, Ordering::AcqRel, Ordering::Acquire);
        if first.is_ok() || first == Err(tree) {
            CAMERA_WRITE_MASK.fetch_or(writer, Ordering::AcqRel);
        }
        return original;
    }
    let epoch = TEMPORAL_EPOCH.load(Ordering::Acquire);
    // SAFETY: the lookup returns a camera history entry of 0x140 bytes.
    let own = unsafe { CAMERA_HISTORY.lock().unwrap_or_else(|e| e.into_inner()).get(tree, key, epoch, eye, original as *const u8) };
    match own {
        Some(own) => {
            HISTORY_READS.fetch_add(1, Ordering::Relaxed);
            own as usize
        }
        None => original,
    }
}

/// This pass reads and writes only its previous-camera block at +0x20..+0xaf.
unsafe extern "system" fn external_pass(object: usize, context: usize) -> usize {
    let _guard = InFlight::enter();
    let eye = history_eye();
    if !(1..=2).contains(&eye) {
        // SAFETY: the original, with the game's arguments.
        return unsafe { EXTERNAL_PASS_ORIGINAL.get()(object, context) };
    }
    let mut bank = EXTERNAL_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    let region = (object + 0x20) as *mut u8;
    // SAFETY: the pass object holds 0x90 bytes of previous camera at +0x20; the swap is undone
    // after the pass, under the bank's lock.
    unsafe {
        let saved = bank.get(object, TEMPORAL_SCENE_KEY.load(Ordering::Acquire), TEMPORAL_EPOCH.load(Ordering::Acquire), eye, region);
        let mut native = [0u8; 0x90];
        if let Some(saved) = saved {
            std::ptr::copy_nonoverlapping(region, native.as_mut_ptr(), 0x90);
            std::ptr::copy_nonoverlapping(saved, region, 0x90);
        }
        let result = EXTERNAL_PASS_ORIGINAL.get()(object, context);
        if let Some(saved) = saved {
            std::ptr::copy_nonoverlapping(region, saved, 0x90);
            std::ptr::copy_nonoverlapping(native.as_ptr(), region, 0x90);
            EXTERNAL_SWAPS.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

/// Sets this scene call's history eye (0 = native history) and starts a new epoch when continuity
/// breaks.
fn begin_temporal_scene(game: usize, token: u32, eye: u32, camera: usize) -> u32 {
    if eye == 1 && TEMPORAL_RESET.swap(false, Ordering::AcqRel) {
        TEMPORAL_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    if eye != 0 {
        let counter = mem::read::<u32>(game + engine::GAME_COUNTER).unwrap_or(0);
        if TEMPORAL_CAMERA.load(Ordering::Relaxed) != camera || counter.wrapping_sub(TEMPORAL_COUNTER.load(Ordering::Relaxed)) > 1 {
            TEMPORAL_EPOCH.fetch_add(1, Ordering::AcqRel);
        }
        TEMPORAL_CAMERA.store(camera, Ordering::Relaxed);
        TEMPORAL_COUNTER.store(counter, Ordering::Relaxed);
    } else {
        TEMPORAL_CAMERA.store(0, Ordering::Relaxed);
        TEMPORAL_EPOCH.fetch_add(1, Ordering::AcqRel);
    }
    CAMERA_WRITE_TREE.store(0, Ordering::Release);
    CAMERA_WRITE_MASK.store(0, Ordering::Release);
    TEMPORAL_SCENE_KEY.store(token, Ordering::Release);
    TEMPORAL_SCENE_EYE.store(eye, Ordering::Release);
    if eye != 0 {
        RENDER_EYE.store(eye, Ordering::Release);
    }
    eye
}

/// Banks the camera the scene just wrote as this eye's previous camera for its next frame.
fn end_temporal_scene(eye: u32, token: u32) {
    TEMPORAL_SCENE_EYE.store(0, Ordering::Release);
    if eye == 0 {
        return;
    }
    let tree = CAMERA_WRITE_TREE.load(Ordering::Acquire);
    if tree != 0 && CAMERA_WRITE_MASK.load(Ordering::Acquire) == ALL_CAMERA_WRITERS {
        // SAFETY: the original lookup on the tree the scene wrote, with this scene's key.
        let current = unsafe { HISTORY_LOOKUP_ORIGINAL.get()(tree, &token) };
        let epoch = TEMPORAL_EPOCH.load(Ordering::Acquire);
        // SAFETY: the lookup returns a camera history entry of 0x140 bytes (or null, refused).
        let stored = current != 0 && unsafe { CAMERA_HISTORY.lock().unwrap_or_else(|e| e.into_inner()).store(tree, token, epoch, eye, current as *const u8) };
        if stored {
            CAMERA_COMMITS.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        CAMERA_INCOMPLETE.fetch_add(1, Ordering::Relaxed);
        TEMPORAL_RESET.store(true, Ordering::Release);
    }
}

// --- The scene ----------------------------------------------------------------------------------------
/// The tracked left eye (or the alternate eye due, or the depth-stereo centre) applied to the
/// camera of this scene, or why not.
fn track_left(game: usize, token: u32, a: usize, b: usize) -> Option<(SceneInput, [f32; 12], [f32; 16])> {
    if !config::get().depth_stereo && crate::output::render12::in_menu() {
        // Menus, the map, inventory and journal are drawn into the frame at the monitor layout:
        // rendered normally and shown flat on the viewer's screen (as depth stereo does).
        MENU_FRAMES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let candidate = last_scene();
    if candidate.game != game || candidate.camera == 0 {
        return None;
    }
    let mut base = snapshot(game, token, a, b, candidate.camera);
    let config = config::get();
    let alternate_next = ALTERNATE_NEXT.load(Ordering::Relaxed);
    let head = if config.alternate_eye && alternate_next == 2 {
        *ALTERNATE_HEAD.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        let head = head::HEAD.current();
        if config.alternate_eye && head.is_some() {
            *ALTERNATE_HEAD.lock().unwrap_or_else(|e| e.into_inner()) = head;
        }
        head
    };
    let head = head.filter(|h| h.valid != 0)?;
    base.head = head;
    if !base.valid || !base.frustum[4].is_finite() || base.frustum[4] <= 0.0 {
        return None;
    }
    base.eyes = camera::eyes_from_head(&base.head)?;
    if !config.depth_stereo && !config.has(debug::OFF_CENTRE) {
        // Each eye rendered with a centred projection covering its field of view, and published
        // with it: some of the game's screen-space shaders assume a centred projection, and an
        // off-centre one leaves dark, smeared patches on near surfaces.
        for (e, eye) in base.eyes.iter_mut().enumerate() {
            eye.fov = camera::centred_fov(&eye.fov);
            base.head.fov[e] = eye.fov;
        }
    }
    // Decoupled aim: the view is built on the game camera without its pitch (only the head looks
    // up and down) while the game keeps its pitched camera for aiming. With head aim the game camera
    // also carries the head's yaw; that share is turned back out.
    let mut view = monaka_core::camera::level(&base.inverse)?;
    aim::turn_out_head_yaw(&mut view, base.counter);
    // Room-scale following: the head's movement the character has walked is in the camera now.
    monaka_arms::roomscale::note_view(&view, base.head.position);
    monaka_arms::roomscale::shift_origin(&mut view);
    let (mut left, mut left_projection) = camera::make_tracked_camera(&view, &base.projection, &base.eyes[0])?;
    (base.right_inverse, base.right_projection) = camera::make_tracked_camera(&view, &base.projection, &base.eyes[1])?;
    let mut applied = base.eyes[0];
    if config.alternate_eye {
        base.alternate_eye = alternate_next;
        if alternate_next == 2 {
            (left, left_projection) = (base.right_inverse, base.right_projection);
            applied = base.eyes[1];
        }
    }
    if config.depth_stereo {
        let (l, r) = (&base.eyes[0], &base.eyes[1]);
        let mut center = *l;
        center.position = base.head.position;
        // Both eyes' field of view and a margin for the sideways shift.
        center.fov = [l.fov[0].min(r.fov[0]) - 0.06, l.fov[1].max(r.fov[1]) + 0.06, l.fov[2].max(r.fov[2]) + 0.02, l.fov[3].min(r.fov[3]) - 0.02];
        // Some screen-space shaders (the sun shadow mask among them) assume a centred projection;
        // an off-centre one darkens screen-aligned blocks, so the centre view is symmetric.
        if !config.has(debug::OFF_CENTRE) {
            center.fov = camera::centred_fov(&center.fov);
        }
        (left, left_projection) = camera::make_tracked_camera(&view, &base.projection, &center)?;
        base.depth_stereo = true;
        base.center_fov = center.fov;
        applied = center;
    }
    apply_eye_frustum(base.camera, &applied);
    // SAFETY: the component setter on the live camera, with a 3x4 matrix.
    unsafe { set_camera_matrix(base.camera, &left) };
    base.rendered_view = left;
    base.rendered_scale = [left_projection[0], left_projection[5]];
    Some((base, left, left_projection))
}

pub unsafe extern "system" fn scene(game: usize, token: u32, a: usize, b: usize) -> usize {
    let _guard = InFlight::enter();
    let previous = local(|l| {
        l.prepared_camera.set(0);
        l.in_scene.replace(true)
    });
    let repeat = local(|l| l.stereo_repeat.get());
    let mut tracked = None;
    if !repeat {
        abandon_pair();
        update_vr_active();
        if vr_active() {
            tracked = track_left(game, token, a, b);
            if let Some((base, _, _)) = &tracked {
                local(|l| {
                    l.tracked_visibility_camera.set(base.camera);
                    l.stereo_repeat.set(true);
                });
            }
        }
    }
    let early = tracked.is_some();
    let (eye, camera) = match &tracked {
        Some((base, _, _)) => (if base.depth_stereo { 0 } else if base.alternate_eye != 0 { base.alternate_eye } else { 1 }, base.camera),
        None => {
            let (repeat, visibility) = local(|l| (l.stereo_repeat.get(), l.tracked_visibility_camera.get()));
            (if repeat && visibility != 0 { 2 } else { 0 }, visibility)
        }
    };
    let history_eye = begin_temporal_scene(game, token, eye, camera);
    if early {
        monaka_producer::log_first!(1, "left-eye scene thread {}", monaka_hook::thread_id());
    }
    let stage = if early { Stage::LeftScene } else if !repeat { Stage::MonoScene } else { Stage::RightScene };
    let started = Instant::now();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SCENE_ORIGINAL.get()(game, token, a, b) };
    time(stage, started.elapsed());
    end_temporal_scene(history_eye, token);
    let prepared = local(|l| l.prepared_camera.get());
    let mut input = snapshot(game, token, a, b, prepared);
    if let Some((mut base, left, left_projection)) = tracked {
        let valid = input.valid && input.camera == base.camera && input.counter == base.counter && camera_matches(&input, &left, &left_projection);
        local(|l| {
            l.stereo_repeat.set(false);
            l.tracked_visibility_camera.set(0);
        });
        if valid {
            base.normal_left = true;
            input = base;
        } else {
            restore_camera(&base);
            input = snapshot(game, token, a, b, local(|l| l.prepared_camera.get()));
            suspend("left-eye camera changed during the scene");
        }
    }
    let repeat_now = local(|l| l.stereo_repeat.get());
    if input.valid && !repeat_now && !aim::MOUSE_PITCH_KNOWN.load(Ordering::Acquire) {
        // forward = -column 2
        aim::GAME_PITCH.store((-input.inverse[6]).clamp(-1.0, 1.0).asin());
        aim::GAME_PITCH_KNOWN.store(true, Ordering::Release);
    }
    set_last_scene(input);
    local(|l| l.in_scene.set(previous));
    result
}

fn remember_prepared_camera(data: usize) {
    let camera = mem::read::<usize>(data + engine::DATA_CAMERA).unwrap_or(0);
    local(|l| l.prepared_camera.set(camera));
}

unsafe extern "system" fn prepare(game: usize, data: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { PREPARE_ORIGINAL.get()(game, data) };
    remember_prepared_camera(data);
    result
}

monaka_hook::caller_shim!(submit_stub => submit);

unsafe extern "system" fn submit(renderer: usize, data: usize, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    // Both scene branches converge here, including the cached menu graph that never calls Prepare.
    if local(|l| l.in_scene.get()) && caller == game::get().engine_at(engine::SUBMIT_FROM_SCENE) {
        remember_prepared_camera(data);
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { SUBMIT_ORIGINAL.get()(renderer, data) }
}

unsafe extern "system" fn queue_phase(phase: u32, data: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    unsafe { QUEUE_PHASE_ORIGINAL.get()(phase, data) }
}

/// Runs the engine's queued GPU work now (D3D11), tagging any present it dispatches with `eye`.
fn drain(eye: u32) {
    let phase = QUEUE_PHASE_ORIGINAL.get();
    local(|l| l.dispatch_eye.set(eye));
    // SAFETY: the engine's queue phase function, as the engine calls it.
    unsafe {
        phase(2, 0);
        local(|l| l.dispatch_eye.set(0));
        phase(0, 0);
        phase(0, 0);
    }
}

monaka_hook::caller_shim!(present_request_stub => present_request);

unsafe extern "system" fn present_request(renderer: usize, a: usize, b: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    let game = game::get();
    // SAFETY (every call of `original` below): the original, with the game's arguments.
    let original = PRESENT_REQUEST_ORIGINAL.get();
    if caller != game.engine_at(engine::NORMAL_PRESENT_CALLER) || local(|l| l.stereo_repeat.get()) {
        return unsafe { original(renderer, a, b) };
    }
    let input = take_left();
    if !input.normal_left {
        return unsafe { original(renderer, a, b) };
    }
    // The left eye is drawn. A frame that cannot become a pair is presented with the camera restored.
    let abort = |why: &str, fatal: bool| {
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if fatal { safety_stop(why) } else { suspend(why) }
        unsafe { original(renderer, a, b) }
    };
    if !vr_active() || !publishing() {
        return abort("stereo switched off", false);
    }
    let mut bank = 0;
    if let Some(r11) = game.renderer11 {
        let driver = mem::read::<usize>(game.engine_at(engine::DRIVER_GLOBAL)).unwrap_or(0);
        if !render11::drain_available(r11) || driver == 0 || mem::read::<usize>(driver) != Some(r11 + engine::DRIVER_VTABLE_11) {
            return abort("renderer backend identity mismatch", true);
        }
        bank = mem::read::<u32>(r11 + engine::QUEUE_BANK_11).unwrap_or(u32::MAX);
        if bank > 1 {
            return abort("invalid queue bank", true);
        }
    }
    let Some((game_object, counter)) = game.counter() else { return abort("game unavailable", true) };
    if input.game != game_object || input.counter != counter || input.a != a || input.b != b {
        return abort("scene changed before present", false);
    }
    // The direct render path runs in engine thread context 2.
    let manager = mem::read::<usize>(game.engine_at(engine::THREAD_CONTEXT_GLOBAL)).and_then(mem::read::<usize>).unwrap_or(0);
    let context = if manager == 0 {
        None
    } else {
        // SAFETY: reads this thread's value of the engine's TLS index.
        mem::read::<u32>(manager + 8).map(|index| unsafe { windows::Win32::System::Threading::TlsGetValue(index) } as usize)
    };
    if context != Some(2) {
        return abort("unexpected engine thread context", true);
    }
    monaka_producer::log_first!(1, "stereo pair thread {}", monaka_hook::thread_id());

    if input.depth_stereo {
        let pair = PAIRS.fetch_add(1, Ordering::Relaxed) + 1;
        local(|l| {
            l.dispatch_pair.set(pair);
            l.eye_copies.set(0);
            l.pair_head.set(input.head);
            l.pair_fov.set(input.center_fov);
            l.pair_aim.set(monaka_core::camera::aim_in_view(&input.inverse, &input.rendered_view, input.rendered_scale));
            l.dispatch_eye.set(3);
        });
        let started = Instant::now();
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        time(Stage::LeftDrain, started.elapsed());
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if local(|l| l.eye_copies.get()) != 1 {
            suspend("depth stereo pair unavailable");
        } else {
            pair_done("depth stereo", input.counter);
        }
        return result;
    }
    if input.alternate_eye != 0 {
        // This frame is one eye. The left frame starts a pair; the right frame publishes it.
        let eye = input.alternate_eye;
        if eye == 1 {
            let pair = PAIRS.fetch_add(1, Ordering::Relaxed) + 1;
            local(|l| l.dispatch_pair.set(pair));
        }
        local(|l| {
            l.eye_copies.set(0);
            l.pair_head.set(input.head);
            l.dispatch_eye.set(eye);
        });
        let started = Instant::now();
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        time(if eye == 1 { Stage::LeftDrain } else { Stage::RightDrain }, started.elapsed());
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if local(|l| l.eye_copies.get()) != 1 {
            suspend("eye image unavailable");
            return result;
        }
        ALTERNATE_NEXT.store(if eye == 1 { 2 } else { 1 }, Ordering::Relaxed);
        if eye == 2 {
            pair_done("alternate-eye", input.counter);
        }
        return result;
    }

    let pair = PAIRS.fetch_add(1, Ordering::Relaxed) + 1;
    local(|l| {
        l.dispatch_pair.set(pair);
        l.eye_copies.set(0);
        l.pair_head.set(input.head);
    });
    // Left eye: on D3D11 the request queues the present and the drain dispatches it now; on D3D12
    // the request presents directly. Either way the eye is copied before its present runs.
    let d3d11 = game.renderer11.is_some();
    let started = Instant::now();
    let result = if d3d11 {
        let result = unsafe { original(renderer, a, b) };
        drain(1);
        result
    } else {
        local(|l| l.dispatch_eye.set(1));
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        result
    };
    time(Stage::LeftDrain, started.elapsed());
    let level = input.camera - engine::LEVEL_CAMERA;
    // SAFETY: the engine functions the pair calls, on the live renderer, camera and level.
    unsafe { (game.renderer_enter)(renderer, std::ptr::null()) };
    local(|l| {
        l.stereo_repeat.set(true);
        l.tracked_visibility_camera.set(input.camera);
    });
    apply_eye_frustum(input.camera, &input.eyes[1]);
    // SAFETY: as above.
    unsafe {
        (game.reset_level)(level);
        set_camera_matrix(input.camera, &input.right_inverse);
    }
    local(|l| l.right_eye_scene.set(true));
    // SAFETY: the scene detour itself, with this frame's scene arguments.
    unsafe { scene(input.game, input.token, input.a, input.b) };
    local(|l| l.right_eye_scene.set(false));
    let second = snapshot(input.game, input.token, input.a, input.b, input.camera);
    let valid = second.valid && second.camera == input.camera && second.counter == input.counter && camera_matches(&second, &input.right_inverse, &input.right_projection);
    // Right eye; a rejected one is still presented but never published.
    let started = Instant::now();
    if d3d11 {
        unsafe { original(renderer, a, b) };
        drain(if valid { 2 } else { 0 });
    } else {
        local(|l| l.dispatch_eye.set(if valid { 2 } else { 0 }));
        unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
    }
    time(Stage::RightDrain, started.elapsed());
    restore_camera(&input);
    // SAFETY: as above.
    unsafe {
        (game.reset_level)(level);
        (game.renderer_leave)(renderer);
    }
    local(|l| {
        l.stereo_repeat.set(false);
        l.tracked_visibility_camera.set(0);
        l.prepared_camera.set(input.camera);
    });
    RENDER_EYE.store(0, Ordering::Release);
    let restored = snapshot(input.game, input.token, input.a, input.b, input.camera);
    let restored_ok = restored.valid
        && restored.camera == input.camera
        && restored.counter == input.counter
        && restored.inverse == input.inverse
        && restored.projection == input.projection
        && game.renderer11.is_none_or(|r11| mem::read::<u32>(r11 + engine::QUEUE_BANK_11) == Some(bank));
    if !restored_ok {
        safety_stop("camera, counter or queue not restored after the pair");
    } else if !valid {
        suspend("right-eye camera changed during the scene");
    } else if local(|l| l.eye_copies.get()) != 2 {
        suspend("eye images unavailable");
    } else {
        pair_done("stereo", input.counter);
    }
    result
}

// --- Shared shadow maps and other once-per-frame work ------------------------------------------------
// The render script holds one JumpToHSM command per shadow map; each jumps into the shadow sub
// when its map is scheduled this frame (returning 1 continues past it). The right eye could keep
// the maps the left eye just drew, but they are fitted to the left eye (its shadows come out
// lighter), and drawing them again costs no measurable time; sharing is a debug flag only.
unsafe extern "system" fn jump_to_hsm(command: usize, context: usize) -> usize {
    if local(|l| l.right_eye_scene.get()) && config::debug(debug::SHARE_SHADOWS) {
        SHADOW_JUMPS_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return 1;
    }
    SHADOW_JUMPS_RUN.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the original, with the game's arguments.
    unsafe { JUMP_TO_HSM_ORIGINAL.get()(command, context) }
}

/// Particle simulation and sorting, wind, ray-tracing structures, heightmaps, environment probes,
/// videos and spot-light shadow maps do not depend on the eye: the right eye's scene skips these
/// commands (execute returns 1, "continue"). Several classes share an execute function, so each
/// skip is decided by the command's own vtable.
fn skip_once(command: usize) -> bool {
    if !local(|l| l.right_eye_scene.get()) || config::debug(debug::ONCE_AGAIN) {
        return false;
    }
    // SAFETY: the engine passes a live command object, whose first field is its vtable.
    let table = unsafe { *(command as *const usize) }.wrapping_sub(game::get().engine);
    let skip = config::get().once_skip;
    engine::ONCE_COMMANDS.iter().enumerate().any(|(i, &(vtable, _, _))| vtable == table && skip & (1 << i) != 0)
}

unsafe extern "system" fn once<const I: usize>(command: usize, b: usize, c: usize, d: usize) -> usize {
    if skip_once(command) {
        ONCE_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return 1;
    }
    // SAFETY: the original, with the game's arguments.
    unsafe { ONCE_ORIGINALS[I].get()(command, b, c, d) }
}

const ONCE_DETOURS: [OnceFn; 9] = [once::<0>, once::<1>, once::<2>, once::<3>, once::<4>, once::<5>, once::<6>, once::<7>, once::<8>];

// --- Visibility follows the tracked eye -------------------------------------------------------------
unsafe extern "system" fn view_setup(level: usize) -> usize {
    let _guard = InFlight::enter();
    let previous = local(|l| l.visibility_setup_level.replace(level));
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { VIEW_SETUP_ORIGINAL.get()(level) };
    local(|l| l.visibility_setup_level.set(previous));
    result
}

fn tracked_camera() -> Option<usize> {
    local(|l| (l.stereo_repeat.get() && l.tracked_visibility_camera.get() != 0).then(|| l.tracked_visibility_camera.get()))
}

monaka_hook::caller_shim!(visibility_camera_stub => visibility_camera);

/// The main visibility setup switches to engine context 1 here, selecting the mouse camera instead
/// of the tracked one; only that call is redirected, for the stereo level.
unsafe extern "system" fn visibility_camera(wrapper: usize, _: usize, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    // SAFETY: the original, with the game's arguments.
    let original = unsafe { VISIBILITY_CAMERA_ORIGINAL.get()(wrapper) };
    match tracked_camera() {
        Some(tracked) if caller == game::get().engine_at(engine::VISIBILITY_CAMERA_CALLER) && wrapper + 0x30 == tracked => tracked,
        _ => original,
    }
}

monaka_hook::caller_shim!(base_camera_copy_stub => base_camera_copy);

unsafe extern "system" fn base_camera_copy(output: usize, mut source: usize, _: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    if caller == game::get().engine_at(engine::BASE_CAMERA_COPY_CALLER)
        && let Some(tracked) = tracked_camera()
        && local(|l| l.visibility_setup_level.get()) + engine::LEVEL_CAMERA == tracked
    {
        source = tracked;
    }
    // SAFETY: the original, with the game's arguments (the source possibly the tracked camera).
    unsafe { BASE_CAMERA_COPY_ORIGINAL.get()(output, source) }
}

/// A visibility result supplied from the mouse camera would cull what the turned head can see.
/// Four arguments, so no shim: the (slower) stack walk, only while the right eye renders.
unsafe extern "system" fn main_visibility(manager: usize, view: u32, camera: usize, mut input: usize) -> usize {
    let _guard = InFlight::enter();
    if view == 20 && tracked_camera().is_some() && monaka_hook::probe::caller() == game::get().engine_at(engine::MAIN_VISIBILITY_CALLER) {
        input = 0;
    }
    // SAFETY: the original, with the game's arguments (no supplied visibility input).
    unsafe { MAIN_VISIBILITY_ORIGINAL.get()(manager, view, camera, input) }
}

monaka_hook::caller_shim!(world_visibility_stub => world_visibility);

unsafe extern "system" fn world_visibility(world: usize, camera: usize, mut input: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    if caller == game::get().engine_at(engine::WORLD_VISIBILITY_CALLER) && local(|l| l.stereo_repeat.get()) && camera == local(|l| l.tracked_visibility_camera.get()) {
        input = 0;
    }
    // SAFETY: as above.
    unsafe { WORLD_VISIBILITY_ORIGINAL.get()(world, camera, input) }
}

// --- Install ------------------------------------------------------------------------------------------
fn signature(what: impl std::fmt::Display) -> Rejection {
    Rejection::revision(format!("signature mismatch: {what}"))
}

/// Checks the engine facts the hooks rely on and prepares the engine hooks.
///
/// # Safety
/// `engine` must be the inspected engine build (`ENGINE_SHA256`).
pub unsafe fn install(hooks: &mut Hooks, engine: &Module) -> Result<(), Rejection> {
    if !engine.bytes_match(SCENE_SUBMIT_SITE.0, SCENE_SUBMIT_SITE.1) {
        return Err(signature("scene submit site"));
    }
    // CmdPPFX_JumpToHSM's vtable must point its execute slot at the function hooked below.
    if mem::read::<usize>(engine.at(JUMP_TO_HSM_VTABLE + 8)) != Some(engine.at(JUMP_TO_HSM.0)) {
        return Err(signature("shadow map jump vtable"));
    }
    for &rva in &CAMERA_WRITER_RETURNS {
        let call = engine.at(rva - 5);
        let target = mem::read::<i32>(call + 1).map(|rel| (call as i64 + 5 + rel as i64) as usize);
        if mem::read::<u8>(call) != Some(0xe8) || target != Some(engine.at(HISTORY_LOOKUP.0)) {
            return Err(signature(format_args!("camera history writer {rva:#x}")));
        }
    }
    for &(vtable, execute, name) in &ONCE_COMMANDS {
        if mem::read::<usize>(engine.at(vtable + 8)) != Some(engine.at(execute)) {
            return Err(signature(format_args!("{name} vtable")));
        }
    }
    // SAFETY: every target is checked against its exact prologue; every detour (or shim stub) has
    // the target's type.
    unsafe {
        let at = |target: &Target| engine.at(target.0);
        hooks.inline(&SCENE_ORIGINAL, "scene", at(&SCENE), SCENE.1, scene as SceneFn)?;
        hooks.inline(&PREPARE_ORIGINAL, "prepare", at(&PREPARE), PREPARE.1, prepare as PrepareFn)?;
        hooks.inline(&SUBMIT_ORIGINAL, "submit", at(&SUBMIT), SUBMIT.1, monaka_hook::probe::as_detour::<SubmitFn>(submit_stub))?;
        hooks
            .inline(&PRESENT_REQUEST_ORIGINAL, "present request", at(&PRESENT_REQUEST), PRESENT_REQUEST.1, monaka_hook::probe::as_detour::<PresentRequestFn>(present_request_stub))
            ?;
        hooks.inline(&VIEW_SETUP_ORIGINAL, "view setup", at(&VIEW_SETUP), VIEW_SETUP.1, view_setup as ViewSetupFn)?;
        hooks
            .inline(&VISIBILITY_CAMERA_ORIGINAL, "visibility camera", at(&VISIBILITY_CAMERA), VISIBILITY_CAMERA.1, monaka_hook::probe::as_detour::<VisibilityCameraFn>(visibility_camera_stub))
            ?;
        hooks
            .inline(&BASE_CAMERA_COPY_ORIGINAL, "base camera copy", at(&BASE_CAMERA_COPY), BASE_CAMERA_COPY.1, monaka_hook::probe::as_detour::<BaseCameraCopyFn>(base_camera_copy_stub))
            ?;
        hooks
            .inline(&MAIN_VISIBILITY_ORIGINAL, "main visibility", at(&MAIN_VISIBILITY), MAIN_VISIBILITY.1, main_visibility as MainVisibilityFn)
            ?;
        hooks
            .inline(&WORLD_VISIBILITY_ORIGINAL, "world visibility", at(&WORLD_VISIBILITY), WORLD_VISIBILITY.1, monaka_hook::probe::as_detour::<WorldVisibilityFn>(world_visibility_stub))
            ?;
        hooks.inline(&JUMP_TO_HSM_ORIGINAL, "shadow map jump", at(&JUMP_TO_HSM), JUMP_TO_HSM.1, jump_to_hsm as CommandFn)?;
        hooks
            .inline(&HISTORY_LOOKUP_ORIGINAL, "camera history", at(&HISTORY_LOOKUP), HISTORY_LOOKUP.1, monaka_hook::probe::as_detour::<HistoryLookupFn>(history_lookup_stub))
            ?;
        hooks.inline(&EXTERNAL_PASS_ORIGINAL, "external history pass", at(&EXTERNAL_PASS), EXTERNAL_PASS.1, external_pass as ExternalPassFn)?;
        hooks.inline(&QUEUE_PHASE_ORIGINAL, "queue phase", at(&QUEUE_PHASE), QUEUE_PHASE.1, queue_phase as QueuePhaseFn)?;
        for (i, target) in ONCE_EXECUTES.iter().enumerate() {
            hooks.inline(&ONCE_ORIGINALS[i], &format!("once-per-frame command {:#x}", target.0), at(target), target.1, ONCE_DETOURS[i])?;
        }
    }
    Ok(())
}

pub fn report() {
    log!(
        "history: camera commits={} incomplete={} own reads={} external swaps={}; shadow map jumps run={}, reused by the right eye={}; once-per-frame commands skipped={}; vertical look inputs neutralised={} (levelling {}); menu frames shown flat={}",
        CAMERA_COMMITS.load(Ordering::Relaxed),
        CAMERA_INCOMPLETE.load(Ordering::Relaxed),
        HISTORY_READS.load(Ordering::Relaxed),
        EXTERNAL_SWAPS.load(Ordering::Relaxed),
        SHADOW_JUMPS_RUN.load(Ordering::Relaxed),
        SHADOW_JUMPS_SKIPPED.load(Ordering::Relaxed),
        ONCE_SKIPPED.load(Ordering::Relaxed),
        aim::pitch_blocked(),
        aim::LEVELLING.load(Ordering::Relaxed),
        MENU_FRAMES.load(Ordering::Relaxed),
    );
}
