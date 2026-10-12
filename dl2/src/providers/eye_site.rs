//! Provider: the eye site (`monaka_stereo::eye_site::EyeSite`), Dying Light 2's side of the
//! same-frame loop (`monaka_stereo::same_frame`, listed as [`crate::PRESENT`]): where each eye's
//! camera and frustum are written into the scene, and how the engine is made to draw both eyes of a
//! pair in one simulation frame. The loop installs it (`EyeSite::install`); the eyes' component is
//! `crate::EYES`.
//!
//! Same-frame stereo: the game's own scene call renders the left eye with the tracked camera; when
//! the engine requests its present, the left eye is presented (the loop takes it at the present
//! entry, `crate::providers::present`), then a second scene call on the same simulation frame
//! renders the right eye, which is presented too. Alternate-eye: one eye per frame, taking turns.
//! Depth stereo: one centre render per frame with both eyes' field of view; its eyes are made from
//! its depth (`crate::extras::depth_stereo`).
//!
//! Each eye keeps its own temporal history (`crate::providers::dlss`, on
//! `monaka_stereo::upscaler::own_dlss`), the right eye reuses the left eye's ray-tracing structures (the one costly eye-independent work that is safe to share),
//! and visibility follows the tracked camera instead of the mouse camera. The head pose is the
//! loop's (its head source); stereo pauses through the loop when a pair cannot be made.
//!
//! Its parts: the scene call and each eye's camera (here), the pair at the present request
//! (`pair`), the work done once per frame (`once`) and visibility (`visibility`).

use crate::config::{self, debug};
use crate::providers::camera;
use crate::providers::engine::{self, *};
use crate::providers::game::{self, read_camera};
use crate::providers::{dlss, look_fields, present, streamline};
use crate::{Dl2, Facts};
use monaka_core::camera::{Base, Frustum};
use monaka_core::protocol::HeadPose;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use monaka_stereo::eye_site::{EyeSite, Tally};
use monaka_stereo::framegen12::pairs::Checks;
use monaka_stereo::same_frame::PAIRS;
use monaka_stereo::same_frame::timing::Stage;
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

mod once;
mod pair;
mod visibility;

use once::*;
use pair::*;
use visibility::*;

/// The eye site's hooks (installed by the same-frame loop): the scene's, each eye's history's,
/// Streamline's imports, the renderer's DLSS calls; and what the run sets for the eyes.
fn install_site(start: &mut Start<'_, Dl2>) -> Result<(), Lack> {
    let Facts { engine, renderer, d3d11, .. } = start.build;
    crate::providers::hud::set_channel(start.channel()?.clone());
    // SAFETY: the engine's and the renderer's hashes were checked.
    unsafe {
        install(start.hooks, engine)?;
        dlss::install(start.hooks, engine, renderer, *d3d11)?;
        // Streamline's imports are decoded, not assumed.
        streamline::install(start.hooks, renderer);
    }
    // With the dynamic HUD the HUD layer goes over each rendered eye at its size and distance
    // (`HudLayer for Dl2`), keeping its size while a wheel is open.
    let config = config::get();
    eng_chr::providers::hud_over::set(config.hud.world.then_some((config.hud.scale, config.hud.distance)));
    eng_chr::providers::hud_over::shrink_for_wheels(false);
    monaka_stereo::framegen12::pairs::set_checks(framegen_checks(&config));
    Ok(())
}

/// What per-eye frame generation is set to for the run's PC checks (`debug_flags`).
fn framegen_checks(config: &config::Config) -> Checks {
    Checks { epoch: dlss::epoch, hud_free: !config.has(debug::NO_HUDLESS), compose: !config.has(debug::NO_HUD_COMPOSE), show_hud_free: config.has(debug::SHOW_HUD_FREE) }
}

/// Stereo starts with the loop: the run's mode logged (the eyes' arm).
pub fn started() {
    let config = config::get();
    let world = config.hud.world && game::try_get().is_some_and(|g| g.renderer12.is_some());
    let mode = match (monaka_stereo::framegen12::pairs::enabled(), config.depth_stereo, config.alternate_eye) {
        (true, _, _) => "same-frame stereo with FSR frame generation per eye",
        (_, true, _) => "depth stereo (one render, both eyes from depth)",
        (_, _, true) => "alternate-eye stereo",
        _ => "same-frame stereo",
    };
    let pose = if config.synthetic.is_some() {
        "a synthetic pose"
    } else if PAIRS.head().connected() {
        "the viewer"
    } else {
        "nowhere (mono)"
    };
    log!("DL2 producer running: {mode}{}; head pose from {pose}", if world { " with the dynamic HUD" } else { "" });
}

/// The eyes' give-back: a left eye whose pair never ran gets its camera back.
pub fn give_back() {
    InFlight::wait_idle(2000);
    abandon_pair();
}

/// The eyes' release: the panels and the depth warp let go, the counts logged.
pub fn release() {
    crate::providers::hud::close();
    crate::extras::depth_stereo::close();
    rate();
}

/// The game's counts beside the loop's (`PairLoop::rate`): the scene's, the renderer's, DLSS's,
/// Streamline's, frame generation's, the markers'.
pub fn rate() {
    report();
    log!(
        "{}; {}; {}; {}",
        monaka_stereo::upscaler::own_dlss::report(),
        crate::extras::depth_stereo::report(),
        streamline::report(),
        monaka_stereo::framegen12::pairs::report()
    );
    crate::extras::markers::report();
}

/// What starts afresh when stereo pauses (`PairLoop::paused`): each eye's history.
pub fn paused() {
    dlss::HISTORY.start_over();
}

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
static ONCE_ORIGINALS: [Original<OnceFn>; 9] = [const { Original::new() }; 9];

/// Scene calls, and the left eyes tracked at them (the loop's rate line).
pub static SCENES: Tally = Tally::new("scene calls");

/// The eye site as the present loops read it: each eye drawn with its own field of view (the one
/// it goes out with), the pair's head pose giving the eyes' separation.
impl EyeSite for Dl2 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        install_site(start)
    }

    fn tally() -> &'static Tally {
        &SCENES
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        Frustum::from_fov(pose.fov[eye.min(1)])
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        Some(pose.ipd)
    }
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
    /// Each eye's field of view as rendered (left, right, up, down; radians).
    fovs: [[f32; 4]; 2],
    inverse: [f32; 12],
    projection: [f32; 16],
    frustum: [f32; 12],
    right_inverse: [f32; 12],
    right_projection: [f32; 16],
}

static LAST_SCENE: Mutex<Option<SceneInput>> = Mutex::new(None);

fn last_scene() -> SceneInput {
    LAST_SCENE.locked().unwrap_or_default()
}

fn set_last_scene(input: SceneInput) {
    *LAST_SCENE.locked() = Some(input);
}

/// The last scene, with its pending left eye taken (it is restored or paired exactly once).
fn take_left() -> SceneInput {
    let mut last = LAST_SCENE.locked();
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
    /// Eye images the loop took for the current pair.
    pub eye_copies: Cell<u32>,
    /// The pair whose presents are being dispatched.
    pub dispatch_pair: Cell<u64>,
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

/// The loop took eye images for the current pair (`count` of them).
pub fn eyes_taken(count: u32) {
    local(|l| l.eye_copies.set(l.eye_copies.get() + count));
}

// --- Frame labels -----------------------------------------------------------------------------------
/// The eye whose frame is being drawn or presented (D3D12 DLSS reads it); 0 outside a pair.
pub static RENDER_EYE: AtomicU32 = AtomicU32::new(0);
/// The head pose the pair being drawn is drawn from (each eye's field of view as rendered).
static FRAME_HEAD: Mutex<HeadPose> = Mutex::new(HeadPose::EMPTY);

/// The eye (0 left) and head pose of the frame being drawn now: `RENDER_EYE`'s, with its pair's
/// pose (any thread). `None` outside a pair's eyes.
pub fn render_frame() -> Option<(usize, HeadPose)> {
    let eye = RENDER_EYE.load(Ordering::Acquire);
    (1..=2).contains(&eye).then(|| ((eye - 1) as usize, *FRAME_HEAD.locked()))
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

/// Writes the field of view `fov` (left, right, up, down; radians) into the camera's frustum.
fn apply_eye_frustum(camera: usize, fov: &[f32; 4]) {
    // SAFETY: a live engine camera (checked by the caller's snapshot); its frustum is 12 floats.
    unsafe {
        let f = std::slice::from_raw_parts_mut((camera + engine::CAMERA_FRUSTUM) as *mut f32, 12);
        let near = f[4];
        f[0] = near * fov[0].tan();
        f[1] = near * fov[1].tan();
        f[2] = near * fov[3].tan();
        f[3] = near * fov[2].tan();
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

static SHADOW_JUMPS_SKIPPED: AtomicU64 = AtomicU64::new(0);
static SHADOW_JUMPS_RUN: AtomicU64 = AtomicU64::new(0);
static ONCE_SKIPPED: AtomicU64 = AtomicU64::new(0);
static MENU_FRAMES: AtomicU64 = AtomicU64::new(0);

// --- The scene ----------------------------------------------------------------------------------------
/// The tracked left eye (or the alternate eye due, or the depth-stereo centre) applied to the
/// camera of this scene, or why not.
fn track_left(game: usize, token: u32, a: usize, b: usize) -> Option<(SceneInput, [f32; 12], [f32; 16])> {
    if !config::get().depth_stereo && crate::providers::hud::in_menu() {
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
    // Alternate-eye: the eye whose turn it is (1 left, 2 right), its pair drawn from one head.
    let (alternate_next, head) = match config.alternate_eye {
        true => {
            let (eye, head) = PAIRS.turns().frame(PAIRS.head().current());
            (eye as u32 + 1, head)
        }
        false => (1, PAIRS.head().current()),
    };
    let head = head.filter(|h| h.valid != 0)?;
    base.head = head;
    if !base.valid || !base.frustum[4].is_finite() || base.frustum[4] <= 0.0 {
        return None;
    }
    if !camera::usable_head(&base.head) {
        return None;
    }
    base.fovs = base.head.fov;
    if !config.depth_stereo && !config.has(debug::OFF_CENTRE) {
        // Each eye rendered with a centred projection covering its field of view, and published
        // with it: some of the game's screen-space shaders assume a centred projection, and an
        // off-centre one leaves dark, smeared patches on near surfaces.
        for (e, fov) in base.fovs.iter_mut().enumerate() {
            *fov = camera::centred_fov(fov);
            base.head.fov[e] = *fov;
        }
    }
    *FRAME_HEAD.locked() = base.head;
    // Decoupled aim: the view is built on the game camera without its pitch (only the head looks
    // up and down) while the game keeps its pitched camera for aiming. With head aim the game camera
    // also carries the head's yaw; that share is turned back out.
    let level = Base::Level { baked_yaw: look_fields::baked_in_view(&base.inverse, base.counter) };
    // Room-scale following: the head's movement the character has walked is in the camera now.
    let game = monaka_arms::roomscale::follow_view(&base.inverse, level, config.head_position.position(&base.head))?;
    // The view centre and the eyes, the head placed by the head model (`head_position`).
    let (centre, [left_eye, right_eye]) = camera::head_cameras(&game, level, &base.head, config.head_position, base.head.ipd)?;
    let (mut left, mut left_projection) = (left_eye, camera::eye_projection(&base.projection, &base.fovs[0])?);
    (base.right_inverse, base.right_projection) = (right_eye, camera::eye_projection(&base.projection, &base.fovs[1])?);
    let mut applied = base.fovs[0];
    if config.alternate_eye {
        base.alternate_eye = alternate_next;
        if alternate_next == 2 {
            (left, left_projection) = (base.right_inverse, base.right_projection);
            applied = base.fovs[1];
        }
    }
    if config.depth_stereo {
        let [l, r] = base.fovs;
        // Both eyes' field of view and a margin for the sideways shift.
        let mut fov = [l[0].min(r[0]) - 0.06, l[1].max(r[1]) + 0.06, l[2].max(r[2]) + 0.02, l[3].min(r[3]) - 0.02];
        // Some screen-space shaders (the sun shadow mask among them) assume a centred projection;
        // an off-centre one darkens screen-aligned blocks, so the centre view is symmetric.
        if !config.has(debug::OFF_CENTRE) {
            fov = camera::centred_fov(&fov);
        }
        (left, left_projection) = (centre, camera::eye_projection(&base.projection, &fov)?);
        base.depth_stereo = true;
        base.center_fov = fov;
        applied = fov;
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
    let call = SCENES.call();
    let previous = local(|l| {
        l.prepared_camera.set(0);
        l.in_scene.replace(true)
    });
    let repeat = local(|l| l.stereo_repeat.get());
    let mut tracked = None;
    if !repeat {
        abandon_pair();
        PAIRS.resume();
        if PAIRS.running() {
            tracked = track_left(game, token, a, b);
            match &tracked {
                Some((base, _, _)) => {
                    SCENES.drawn();
                    local(|l| {
                        l.tracked_visibility_camera.set(base.camera);
                        l.stereo_repeat.set(true);
                    });
                }
                None => SCENES.refused(call, "no pair this frame (a menu, no head pose, or no usable camera)"),
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
    let history_eye = dlss::begin_scene(game, token, eye, camera);
    if eye != 0 {
        RENDER_EYE.store(eye, Ordering::Release);
    }
    if early {
        monaka_producer::log_first!(1, "left-eye scene thread {}", monaka_hook::thread_id());
    }
    let stage = if early { Stage::LeftScene } else if !repeat { Stage::MonoScene } else { Stage::RightScene };
    let started = Instant::now();
    // SAFETY: the original, with the game's arguments.
    let result = unsafe { SCENE_ORIGINAL.get()(game, token, a, b) };
    PAIRS.time(stage, started.elapsed());
    dlss::end_scene(history_eye, token);
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
            PAIRS.pause("left-eye camera changed during the scene");
        }
    }
    let repeat_now = local(|l| l.stereo_repeat.get());
    if input.valid && !repeat_now {
        // forward = -column 2; the game's pitch until a player camera update gives it.
        look_fields::AIM.note_scene_pitch((-input.inverse[6]).clamp(-1.0, 1.0).asin());
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
        hooks.inline(&QUEUE_PHASE_ORIGINAL, "queue phase", at(&QUEUE_PHASE), QUEUE_PHASE.1, queue_phase as QueuePhaseFn)?;
        for (i, target) in ONCE_EXECUTES.iter().enumerate() {
            hooks.inline(&ONCE_ORIGINALS[i], &format!("once-per-frame command {:#x}", target.0), at(target), target.1, ONCE_DETOURS[i])?;
        }
    }
    Ok(())
}

/// The scene's counts: the history, the work shared or run again for the right eye, the pitch
/// lock's, the menu frames shown flat.
pub fn report() {
    log!(
        "{}; {}; shadow map jumps run={}, reused by the right eye={}; once-per-frame commands skipped={}; vertical look inputs neutralised={} (levelling {}); menu frames shown flat={}",
        SCENES.summary(),
        dlss::HISTORY.report(),
        SHADOW_JUMPS_RUN.load(Ordering::Relaxed),
        SHADOW_JUMPS_SKIPPED.load(Ordering::Relaxed),
        ONCE_SKIPPED.load(Ordering::Relaxed),
        look_fields::AIM.pitch_blocked() + monaka_arms::pitch_lock::blocked(),
        monaka_arms::pitch_lock::levelling(),
        MENU_FRAMES.load(Ordering::Relaxed),
    );
}

#[cfg(test)]
mod lift_checks;
