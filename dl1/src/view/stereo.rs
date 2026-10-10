//! Alternate-eye stereo, in place: the player camera stays on the game's view. The shared present
//! loop ([`monaka_stereo::driver`], `DRIVER`) chooses the next eye and head pose after each present
//! (DL1's own work around that: [`after_present`]); the view setup stage moves the cached render
//! camera to that eye just before the renderer reads it ([`view_setup`]); the frame then shows
//! `latency` presents later, where the driver's publisher copies it and, with its partner eye,
//! publishes it with the poses both were rendered from (the upscaler and the hybrid publish
//! their own: [`before_present`]).

use crate::player::aim;
use crate::config::{Config, widened_fov};
use crate::engine::{self, Camera, CameraWriter, SetFovFn, ViewStageFn};
use crate::hud::draws;
use monaka_channel::ChannelName;
use monaka_channel::hands::HandChannel;
use monaka_channel::pose::PoseChannel;
use monaka_core::alternate::Schedule;
use monaka_core::camera::{self, Frustum, Mat34, apply_head, distance, parallel_eyes, rigid_inverse, turn};
use monaka_core::protocol::{HandPose, HeadPose, LEFT_HAND, RIGHT_HAND};
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::log;
use monaka_core::depth::DepthMapping;
use monaka_stereo::alternate::AlternatePublisher;
use monaka_stereo::driver::{Driver, Install, Publisher};
use monaka_stereo::hybrid::EyeCameras;
use monaka_stereo::present::PresentFn;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::HRESULT;

/// The present loop: the game's swapchain, the eye and pose of the next frame, the pairs.
pub static DRIVER: Driver = Driver::new();

/// Whether the run captures (eyes are chosen at presents).
pub fn capturing() -> bool {
    DRIVER.capturing()
}

/// The eye whose camera the next view setup writes; -1 when not running.
pub fn eye() -> i32 {
    DRIVER.eye()
}

pub struct Setup {
    pub config: Config,
    pub camera: CameraWriter,
    pub channel: ChannelName,
}

static SETUP: OnceLock<Setup> = OnceLock::new();

pub fn install(setup: Setup, pose_channel: Option<PoseChannel>, hand_channel: Option<HandChannel>) {
    let publisher = AlternatePublisher::new(setup.channel.clone(), setup.config.schedule);
    *HANDS.lock().unwrap_or_else(|e| e.into_inner()) = HandsCell(hand_channel);
    DRIVER.install(Install {
        shared: setup.config.shared,
        publisher: Publisher::D3D11(publisher),
        poses: pose_channel,
        frame_end: Some(draws::frame_end),
        report: Some(report),
        work: ("camera shifts", &COUNTERS.shifts),
        before: Some(before_present),
        after: Some(after_present),
        adjust_pose: None,
        finish: Some(finish),
    });
    let _ = SETUP.set(setup);
}

fn setup() -> &'static Setup {
    SETUP.get().expect("installed before the hooks are enabled")
}

pub fn config() -> &'static Config {
    &setup().config
}

pub fn schedule() -> Option<Schedule> {
    SETUP.get().map(|s| s.config.schedule)
}

/// The frustum eye `eye` is rendered with for `pose` (see [`Config::rendered_frustum`]).
pub fn rendered_frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
    SETUP.get()?.config.rendered_frustum(pose, eye)
}

/// Present number `n` counted from attachment, for the frame currently being drawn (the one the
/// next present shows).
pub fn drawing_present() -> Option<u64> {
    DRIVER.drawing_present()
}

/// The head pose for the next frame (read by the view setup, the HUD and SetFOV), and the live
/// camera bookkeeping.
struct Poses {
    pending: HeadPose,
    /// The present after which the next frame's eye was chosen, and that eye: written with
    /// `pending` so a view setup reads one frame's eye, key and pose together.
    chosen: (u64, usize),
    /// Each controller (`LEFT_HAND`, `RIGHT_HAND`) when the viewer located it recently: its aim pose
    /// and its grip pose. Hand aim follows `Config::aim_hand`'s.
    hands: [Option<HandPose>; 2],
    grips: [Option<HandPose>; 2],
    live: LiveCamera,
}

/// The live player camera as the game last set it and as it was turned, so a turn is not
/// stacked on a turn and the camera is given back at the end.
#[derive(Default)]
struct LiveCamera {
    interface: usize,
    base: Option<Mat34>,
    bases: [Mat34; TURNS],
    turned: [Mat34; TURNS],
    /// The pair whose head pose each turn was made with (its first present).
    pairs: [u64; TURNS],
    /// The aim's yaw each turn's base carried.
    baked: [f32; TURNS],
    history: usize,
}

/// The live turns remembered: the render cache can hold one a few frames old, and a frame makes
/// two (after the game's update and at the present).
const TURNS: usize = 8;

static POSES: Mutex<Poses> =
    Mutex::new(Poses { pending: HeadPose::EMPTY, chosen: (0, 0), hands: [None; 2], grips: [None; 2], live: LiveCamera { interface: 0, base: None, bases: [[0.0; 12]; TURNS], turned: [[0.0; 12]; TURNS], pairs: [0; TURNS], baked: [0.0; TURNS], history: 0 } });
static LIVE_INTERFACE: AtomicUsize = AtomicUsize::new(0);

/// Research (`probe_widget`): where the live player camera looks right now (yaw and pitch,
/// degrees; yaw positive left), and whether that is a turn the view wrote.
pub fn live_camera_look() -> Option<(f32, f32, bool)> {
    let view = IN_PLACE_VIEW.load(Relaxed);
    let (_, _, camera) = (view != 0).then(|| engine::live_camera(view)).flatten()?;
    let m = camera.inverse;
    // Camera-to-world: column 2 is the back axis.
    let (yaw, pitch) = (m[2].atan2(m[10]).to_degrees(), (-m[6]).clamp(-1.0, 1.0).asin().to_degrees());
    let turned = POSES.lock().ok().is_some_and(|p| (0..p.live.history.min(TURNS)).any(|i| distance(&m, &p.live.turned[i]) < 1e-4));
    Some((yaw, pitch, turned))
}

/// The player camera, for the SetFOV widening (head aim knows it from the game's own call).
pub fn set_player_camera(interface: usize) {
    LIVE_INTERFACE.store(interface, Relaxed);
}

/// The game's own camera for `matrix`, a camera-to-world read from the player camera: when it is
/// one this producer turned to the view (the present thread turns the live camera, and can do so
/// between the game setting it and the game's later reads in the same update), the camera the game
/// had set before that turn; otherwise `matrix` itself.
pub fn game_camera(matrix: &Mat34) -> Mat34 {
    let Ok(poses) = POSES.lock() else { return *matrix };
    let live = &poses.live;
    (0..live.history.min(TURNS)).find(|&i| distance(matrix, &live.turned[i]) < 1e-5).map_or(*matrix, |i| live.bases[i])
}

/// The player camera interface, once head aim or the live-camera turn has seen it (0 before).
pub fn player_camera() -> usize {
    LIVE_INTERFACE.load(Relaxed)
}

pub fn pending_pose() -> HeadPose {
    POSES.lock().map(|p| p.pending).unwrap_or_default()
}

/// Controller `side` (`LEFT_HAND` or `RIGHT_HAND`), if it is tracked (read every present).
pub fn pending_hand_of(side: usize) -> Option<HandPose> {
    POSES.lock().ok().and_then(|p| p.hands.get(side).copied().flatten())
}

/// Where a weapon held in controller `side`'s hand (`LEFT_HAND` or `RIGHT_HAND`) sits and points:
/// the grip pose's position (the palm) with the aim pose's direction (where a relaxed hand points;
/// the grip's own forward runs along a straightened index finger, which bent the wrist to aim
/// level). Either alone stands in for both.
pub fn pending_palm_of(side: usize) -> Option<HandPose> {
    let poses = POSES.lock().ok()?;
    HandPose::palm(*poses.hands.get(side)?, *poses.grips.get(side)?)
}

/// A controller not located for this long is off or asleep: head aim takes over.
const HAND_MAX_AGE_MS: u64 = 250;

/// The controllers' hand block, read at each present.
struct HandsCell(Option<HandChannel>);
// SAFETY: the shared-memory view inside is used by one thread at a time, under the mutex: the
// game's present thread, or the stop after its hooks are off.
unsafe impl Send for HandsCell {}

static HANDS: Mutex<HandsCell> = Mutex::new(HandsCell(None));

/// The view setup stage's own bookkeeping, and what it changed that the end must restore.
struct ViewState {
    last_center: Option<Camera>,
    last_written: Mat34,
    /// Views whose raster occlusion was switched off, with the switch's own value.
    occlusion: Vec<Changed<u8>>,
    /// Levels whose mesh and shadow cull limits are relaxed, with their own limits.
    culling: Vec<Changed<(f32, f32)>>,
    /// The level whose limits did not read as plausible (left alone).
    culling_rejected: Option<usize>,
    frustum: [f32; 5],
}

static VIEW: Mutex<ViewState> = Mutex::new(ViewState {
    last_center: None,
    last_written: [0.0; 12],
    occlusion: Vec::new(),
    culling: Vec::new(),
    culling_rejected: None,
    frustum: [0.0; 5],
});

/// Presents a changed level or view may go without a view setup and still be written to: past that
/// it is not being rendered (a loading screen, another area, freed) and is left alone.
const ACTIVE_PRESENTS: u64 = 4;
/// At the end, objects rendered this recently get their values back (the pause menu still renders
/// the world; a level last rendered before a load is gone).
const RESTORE_PRESENTS: u64 = 120;
/// Changed objects remembered at once; the oldest is forgotten, unwritten, past this.
const CHANGED_MAX: usize = 8;

/// A game object the run changed (a level's cull limits, a view's occlusion switch): what it held
/// before, when the view setup last rendered it, and its vtable when it has one in loaded code. A
/// level or view the game dropped can be freed heap by now, which `WriteProcessMemory` writes into
/// without complaint; the heap reuses a freed block's first bytes, where the vtable was.
#[derive(Clone, Copy)]
struct Changed<T> {
    object: usize,
    vtable: Option<usize>,
    saved: T,
    seen: u64,
}

impl<T: Copy> Changed<T> {
    fn same_object(&self) -> bool {
        self.vtable.is_none_or(|v| mem::read::<usize>(self.object) == Some(v))
    }

    fn rendered_within(&self, presents: u64, now: u64) -> bool {
        now.saturating_sub(self.seen) <= presents && self.same_object()
    }
}

/// The entry for `object`, marked rendered at present `now`. What it holds is read with `capture`
/// only the first time: later the value is ours, not the game's.
fn remember<T: Copy>(list: &mut Vec<Changed<T>>, object: usize, now: u64, capture: impl FnOnce() -> Option<T>) -> Option<&mut Changed<T>> {
    if let Some(i) = list.iter().position(|c| c.object == object) {
        if list[i].same_object() {
            list[i].seen = now;
            return Some(&mut list[i]);
        }
        // Another object at the same address: the old one is gone; forget it unwritten.
        list.remove(i);
    }
    let vtable = monaka_hook::module::plausible_object(object, 1).then(|| mem::read::<usize>(object)).flatten();
    let saved = capture()?;
    if list.len() >= CHANGED_MAX {
        list.remove(0);
    }
    list.push(Changed { object, vtable, saved, seen: now });
    list.last_mut()
}
static IN_PLACE_VIEW: AtomicUsize = AtomicUsize::new(0);

struct Counters {
    shifts: AtomicU64,
    shift_failures: AtomicU64,
    head_applied: AtomicU64,
    head_reapplied: AtomicU64,
    live_turns: AtomicU64,
    widened_fov_bits: AtomicU32,
}
static COUNTERS: Counters = Counters {
    shifts: AtomicU64::new(0),
    shift_failures: AtomicU64::new(0),
    head_applied: AtomicU64::new(0),
    head_reapplied: AtomicU64::new(0),
    live_turns: AtomicU64::new(0),
    widened_fov_bits: AtomicU32::new(0),
};

pub static PRESENT_ORIGINAL: Original<PresentFn> = Original::new();
pub static VIEW_SETUP_ORIGINAL: Original<ViewStageFn> = Original::new();
pub static SET_FOV_ORIGINAL: Original<SetFovFn> = Original::new();

/// `IDXGISwapChain::Present`.
pub unsafe extern "system" fn present(swap: *mut core::ffi::c_void, interval: u32, flags: u32) -> HRESULT {
    // SAFETY: the game passes its live swapchain; the original is DXGI's Present.
    unsafe { DRIVER.present(swap, interval, flags, PRESENT_ORIGINAL.get()) }
}

/// The pose each eye of a published pair goes out with: the frustum each image was actually
/// rendered with (the engine's own without the headset projection, the centred envelope with the
/// symmetric experiment) and the eye distance used.
fn report(_: &Driver, used: &mut HeadPose) {
    let config = &setup().config;
    let frustum = VIEW.lock().map(|v| v.frustum).unwrap_or_default();
    used.ipd = config.separation_for(used);
    if !config.headset_fov {
        if let Some(engine) = Frustum::from_extents([frustum[0], frustum[1], frustum[2], frustum[3]], frustum[4]) {
            used.fov = [engine.fov(); 2];
        }
    } else if config.symmetric_projection {
        let rendered = [config.rendered_frustum(used, 0), config.rendered_frustum(used, 1)];
        for (fov, frustum) in used.fov.iter_mut().zip(rendered) {
            if let Some(frustum) = frustum {
                *fov = frustum.fov();
            }
        }
    }
}

/// Before present `n`: the upscaler and the hybrid publish their own pairs (true); otherwise the
/// driver's publisher copies this frame's eye.
fn before_present(driver: &Driver, chain: &IDXGISwapChain, n: u64) -> bool {
    let config = &setup().config;
    let report = |used: &mut HeadPose| report(driver, used);
    if crate::output::upscale::enabled() {
        // The upscaled eye with the HUD layer over it, in place of the back buffer.
        if let Ok((image, device, context)) = monaka_channel::d3d::swapchain_parts(chain) {
            // A frame without HUD draws was not upscaled at its scene copy: do it now.
            crate::output::upscale::capture(&context, &image, n);
            driver.with_publisher11(|publisher| crate::output::upscale::publish(&device, &context, &image, n, publisher, driver.head(), report));
        }
        return true;
    }
    if crate::output::hybrid::enabled() {
        // A pair every frame: this frame's real eye and the other eye reprojected from it.
        if let Ok((image, device, context)) = monaka_channel::d3d::swapchain_parts(chain) {
            // A frame without HUD draws was not captured at its scene copy: take it now.
            crate::output::hybrid::capture(&context, &image, n);
            let size = monaka_channel::d3d::texture_desc(&image);
            let rects = draws::eye_rects(&pending_pose(), size.Width, size.Height);
            crate::output::hybrid::publish(&device, &context, &image, driver.head(), rects, report);
        }
        return true;
    }
    // No world drawn for a few frames (a movie, a loading screen): to the flat screen.
    if !world_drawn_recently()
        && let Ok((image, device, context)) = monaka_channel::d3d::swapchain_parts(chain)
    {
        driver.with_publisher11(|publisher| crate::output::flat::publish(&device, &context, &image, n, publisher, driver.head()));
        return true;
    }
    note_label(n, config.schedule);
    false
}

/// Each eye's last two view centres and how each was made (true: from the turned live camera).
static STEPS: Mutex<[[([f32; 3], bool); 2]; 2]> = Mutex::new([[([f32::NAN; 3], false); 2]; 2]);
/// Frames, by eye, whose view centre stepped back against the eye's own motion (moving over 5 mm
/// a frame): [made like the frame before, made the other way].
static REVERSALS: [[AtomicU64; 2]; 2] = [const { [const { AtomicU64::new(0) }; 2] }; 2];
static MOVING: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Measures each eye's view centre, frame to frame: a centre stepping back against its own motion
/// is a jitter (a position going back and forth, seen most on near things).
fn note_step(eye: usize, center: &Mat34, turned: bool) {
    let eye = eye.min(1);
    let now = [center[3], center[7], center[11]];
    let Ok(mut steps) = STEPS.lock() else { return };
    let [(older, _), (last, last_turned)] = steps[eye];
    let step = |a: [f32; 3], b: [f32; 3]| [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let (before, after) = (step(older, last), step(last, now));
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length(before) > 0.005 && length(after) > 0.005 {
        MOVING[eye].fetch_add(1, Relaxed);
        let dot = before[0] * after[0] + before[1] * after[1] + before[2] * after[2];
        if dot < 0.0 {
            let switched = turned != last_turned;
            let n = REVERSALS[eye][switched as usize].fetch_add(1, Relaxed);
            if n < 6 {
                log!(
                    "eye {eye} view stepped back: {:.3} m then {:.3} m against it ({} -> {})",
                    length(before),
                    length(after),
                    if last_turned { "turned live camera" } else { "rebuilt at view setup" },
                    if turned { "turned live camera" } else { "rebuilt at view setup" }
                );
            }
        }
    }
    steps[eye] = [(last, last_turned), (now, turned)];
}

/// Each eye's last two view yaws (radians, positive left).
static YAWS: Mutex<[[f32; 2]; 2]> = Mutex::new([[f32::NAN; 2]; 2]);
/// Frames, by eye, whose view yaw left the line of the two before by over half a degree (a head
/// turning smoothly turns the view smoothly):
/// [made from the turned live camera, rebuilt at view setup]; and frames measured.
static YAW_JUMPS: [[AtomicU64; 2]; 2] = [const { [const { AtomicU64::new(0) }; 2] }; 2];
static YAW_FRAMES: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

fn note_yaw(eye: usize, center: &Mat34, turned: bool) {
    let eye = eye.min(1);
    // Camera-to-world: column 2 is the back axis.
    let yaw = center[2].atan2(center[10]);
    let Ok(mut yaws) = YAWS.lock() else { return };
    let [older, last] = yaws[eye];
    let wrap = monaka_core::math::wrap_radians;
    if older.is_finite() {
        YAW_FRAMES[eye].fetch_add(1, Relaxed);
        let off_line = wrap(yaw - last) - wrap(last - older);
        if off_line.abs() > 0.5f32.to_radians() {
            let n = YAW_JUMPS[eye][(!turned) as usize].fetch_add(1, Relaxed);
            if n < 6 {
                log!(
                    "eye {eye} view yaw left its line by {:.2} degrees (steps {:.2} then {:.2}; {})",
                    off_line.to_degrees(),
                    wrap(last - older).to_degrees(),
                    wrap(yaw - last).to_degrees(),
                    if turned { "turned live camera" } else { "rebuilt at view setup" }
                );
            }
        }
    }
    yaws[eye] = [last, yaw];
}

/// Eye cameras made from a turned live camera, by eye: [turned with this frame's pair's head pose,
/// with an older one]. Both eyes of a pair share the pose sampled at its left-eye present; a
/// left-eye frame whose camera was turned a present earlier carries the previous pair's head.
static TURN_PAIRS: [[AtomicU64; 2]; 2] = [const { [const { AtomicU64::new(0) }; 2] }; 2];

fn note_turn_pair(eye: usize, pair: u64, turned_for: u64) {
    let older = turned_for != pair;
    TURN_PAIRS[eye.min(1)][older as usize].fetch_add(1, Relaxed);
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    if older && LOGGED.fetch_add(1, Relaxed) < 12 {
        log!("eye {eye} of the pair from present {pair} was drawn from a live camera turned with the head pose of the pair from present {turned_for}");
    }
}

/// The choice the latest eye camera written was rendered from: the present it was made after
/// (`<< 1`) and the eye, `u64::MAX` before the first; and the thread that wrote it.
static RENDERED: AtomicU64 = AtomicU64::new(u64::MAX);
static RENDER_THREAD: AtomicU32 = AtomicU32::new(0);
/// Presents by how many presents after its choice the latest eye camera written was (0..=4, then
/// 5 for later); the schedule assumes `latency`.
static LABEL_LAGS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
static LABEL_MISSES: AtomicU64 = AtomicU64::new(0);

/// Measures the schedule against what was rendered: at present `n` the image is the eye the
/// schedule says only if the latest eye camera written was chosen `latency` presents earlier. An
/// image drawn from the other eye's position makes near things jitter.
fn note_label(n: u64, schedule: Schedule) {
    let rendered = RENDERED.load(Acquire);
    if rendered == u64::MAX || schedule.shown_eye(n).is_none() {
        return;
    }
    let (chosen, eye) = (rendered >> 1, (rendered & 1) as usize);
    let lag = n.saturating_sub(chosen);
    LABEL_LAGS[lag.min(5) as usize].fetch_add(1, Relaxed);
    if lag != schedule.latency && LABEL_MISSES.fetch_add(1, Relaxed) < 12 {
        log!(
            "present {n} shows the eye-{eye} camera chosen after present {chosen} ({lag} presents back, not {}): the schedule calls it eye {}; render thread {}, present thread {}",
            schedule.latency,
            schedule.eye_at(n),
            RENDER_THREAD.load(Relaxed),
            monaka_hook::thread_id()
        );
    }
}

/// After present `n`, with the pair's head `pose` chosen and recorded by the driver: the
/// culling relaxed again, the stick kept from the game, the controllers read, the live camera
/// turned to the view, and what the next view setup draws from.
fn after_present(_: &Driver, n: u64, pose: &HeadPose) {
    let setup = setup();
    apply_mesh_culling(setup.config.mesh_cull_scale);
    if setup.config.aim.head {
        aim::keep_stick();
    }
    let (hands, grips) = {
        let mut cell = HANDS.lock().unwrap_or_else(|e| e.into_inner());
        let mut read = |side: usize| match cell.0.as_mut() {
            Some(h) => (h.read(side, HAND_MAX_AGE_MS), h.read_grip(side, HAND_MAX_AGE_MS)),
            None => (None, None),
        };
        let (left, right) = (read(LEFT_HAND), read(RIGHT_HAND));
        ([left.0, right.0], [left.1, right.1])
    };
    let pose = *pose;
    suggest_render_size(&setup.config, &pose);
    // Climbing, the game's camera is left as the game set it: hanging, the game turns the
    // character by where its camera looks ([`monaka_arms::traverse`]). With no turn on record the
    // view centre is made from the game's camera at render.
    if setup.config.turn_live_camera && pose.valid != 0 {
        if monaka_arms::traverse::traversing() {
            if let Ok(mut poses) = POSES.lock() {
                poses.live.history = 0;
                poses.live.base = None;
            }
        } else {
            let steering = setup.config.aim.head && aim::share_in_view();
            turn_live_camera(&setup.camera, &pose, n & !1, steering, steering && setup.config.aim.hand);
        }
    }
    if let Ok(mut poses) = POSES.lock() {
        poses.pending = pose;
        poses.chosen = (n, Schedule::camera_eye(n));
        poses.hands = hands;
        poses.grips = grips;
    }
}

/// Once per run: the game resolution whose aspect matches the eye frustum actually rendered.
/// The image is 16:9 for a roughly square eye view, which wastes pixels and sharpness; set it with
/// `tools\Set-DL1Resolution.ps1` (needs a game restart).
fn suggest_render_size(config: &Config, pose: &HeadPose) {
    static SUGGESTED: AtomicBool = AtomicBool::new(false);
    if SUGGESTED.load(Relaxed) {
        return;
    }
    let height = DRIVER.with_publisher11(|p| p.size().1).unwrap_or(0);
    if let Some(frustum) = config.rendered_frustum(pose, 0)
        && height > 0
        && !SUGGESTED.swap(true, Relaxed)
    {
        let (width, height) = frustum.render_size(height);
        log!("eye frustum aspect {:.3}: a {width}x{height} game resolution would match it", width as f32 / height as f32);
    }
}

/// The view centre (between the eyes) for the game camera `base` and head `pose`. With head aim
/// steering, `base` carries the aim (head or hand) and is levelled with that yaw turned back out
/// first, as in [`shift_render_camera`].
///
/// With hand aim the character's pitch is the controller's, and the game leans its camera with
/// that pitch (0.37 m back at 60 degrees up): `unlean` takes the lean back out, so the eyes stay
/// put and only the tracked head moves them.
/// `baked` is the aim's yaw `base` carries ([`aim::baked_yaw`] as the game built it).
fn view_center(base: &Mat34, pose: &HeadPose, head_aim: bool, unlean: bool, baked: f32) -> Mat34 {
    let origin = origin_with(base, head_aim, unlean, baked);
    monaka_arms::roomscale::note_view(&origin, pose.position);
    crate::player::controls::note_head(pose.position[1]);
    apply_head(&origin, pose.orientation, pose.position)
}

/// Where the headset's tracking space sits in the world for the game camera `base` (see
/// [`view_center`]): any tracked pose, head or controller, lands in the world through it.
pub fn tracking_origin(base: &Mat34, head_aim: bool, unlean: bool) -> Mat34 {
    origin_with(base, head_aim, unlean, aim::baked_yaw())
}

fn origin_with(base: &Mat34, head_aim: bool, unlean: bool, baked: f32) -> Mat34 {
    let mut base = *base;
    if unlean {
        // Camera-to-world: column 2 is the back axis, column 3 the position.
        let pitch = (-base[6]).clamp(-1.0, 1.0).asin().to_degrees();
        let (forward, up) = engine::camera_lean(pitch);
        let (x, z) = (-base[2], -base[10]);
        let length = (x * x + z * z).sqrt();
        if length > 1e-4 {
            base[3] -= x / length * forward;
            base[11] -= z / length * forward;
            base[7] -= up;
        }
    }
    let mut origin = match head_aim.then(|| camera::level(&base)).flatten() {
        Some(flat) => turn(&flat, -baked),
        None => base,
    };
    // Climbing, backed off the wall to see the climb.
    if let Some((distance, [x, z])) = aim::climb_pullback() {
        origin[3] += x * distance;
        origin[11] += z * distance;
    }
    // Room-scale following: the head's movement the character has walked is in the camera now.
    monaka_arms::roomscale::shift_origin(&mut origin);
    origin
}

/// A camera the view would write is finite and within this of the game's own (metres): the head,
/// the climb pullback and the eyes move it well under that. The game looks things up near its
/// camera, and a position not finite or far out crashes it.
const CAMERA_REACH: f32 = 5.0;

fn plausible_camera(written: &Mat34, game: &Mat34) -> bool {
    let apart = [written[3] - game[3], written[7] - game[7], written[11] - game[11]];
    written.iter().all(|v| v.is_finite()) && apart.iter().map(|v| v * v).sum::<f32>().sqrt() < CAMERA_REACH
}

fn note_implausible_camera(what: &str, written: &Mat34, game: &Mat34) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Relaxed) + 1;
    if n <= 5 || n.is_power_of_two() {
        log!("{n}x a camera would have been written implausibly; left as the game set it (first: {what} {written:?} for the game's {game:?})");
    }
}

/// The game just built the player camera, on its own thread, before laying out its HUD: turned to
/// the view now too, as each present turns it. Turned only at presents (on the render thread), the
/// game's HUD layout would see its own camera (the hand's, with hand aim) or the view's by thread
/// timing, and the interact prompt it projects onto what you look at would flicker between the two
/// places (with the head aiming the two are one).
pub fn turn_after_game_update() {
    let setup = setup();
    if !setup.config.turn_live_camera || !setup.config.turn_on_update || DRIVER.finished() || !DRIVER.capturing() || monaka_arms::traverse::traversing() {
        return;
    }
    let Some((pose, (chosen_at, _))) = POSES.lock().ok().map(|p| (p.pending, p.chosen)) else { return };
    if pose.valid == 0 {
        return;
    }
    let steering = setup.config.aim.head && aim::share_in_view();
    turn_live_camera(&setup.camera, &pose, chosen_at & !1, steering, steering && setup.config.aim.hand);
}

/// Turns the live player camera to the view centre too, right after the game set it, so
/// game-side visibility (culling, shadows) follows the head. With hand aim the game camera points
/// along the controller instead; the character keeps its own look angles, so aiming stays with
/// the hand. Doing it inside the game's own camera calls makes "forward" wrong.
fn turn_live_camera(writer: &CameraWriter, pose: &HeadPose, pair: u64, head_aim: bool, unlean: bool) {
    let view = IN_PLACE_VIEW.load(Relaxed);
    let Some((interface, _, camera)) = (view != 0).then(|| engine::live_camera(view)).flatten() else { return };
    let Ok(mut poses) = POSES.lock() else { return };
    let live = &mut poses.live;
    let last = live.history.wrapping_sub(1) % TURNS;
    // Still our own turn from last time: turn the game's camera, not ours again.
    let base = match live.base {
        Some(base) if live.history > 0 && camera.inverse == live.turned[last] => base,
        _ => camera.inverse,
    };
    let baked = aim::baked_yaw();
    let turned = view_center(&base, pose, head_aim, unlean, baked);
    if !plausible_camera(&turned, &base) {
        note_implausible_camera("live camera", &turned, &base);
        return;
    }
    let slot = live.history % TURNS;
    live.bases[slot] = base;
    live.turned[slot] = turned;
    live.pairs[slot] = pair;
    live.baked[slot] = baked;
    live.history += 1;
    live.base = Some(base);
    live.interface = interface;
    drop(poses);
    LIVE_INTERFACE.store(interface, Relaxed);
    writer.on_interface(interface, &turned);
    COUNTERS.live_turns.fetch_add(1, Relaxed);
}

/// The present of the last view setup that wrote an eye camera: the world was drawn then.
static LAST_VIEW: AtomicU64 = AtomicU64::new(0);
/// Presents without a view setup after which the world is not being drawn (a loading screen).
const WORLD_GONE_AFTER: u64 = 8;

/// Whether the world was drawn within the last few presents (not a loading screen, where levels
/// are unloaded and their UI freed).
pub fn world_drawn_recently() -> bool {
    DRIVER.presents().saturating_sub(LAST_VIEW.load(Relaxed)) <= WORLD_GONE_AFTER
}

/// The view setup stage `(level, view)`.
pub unsafe extern "system" fn view_setup(level: *mut core::ffi::c_void, view: *mut core::ffi::c_void) {
    let _flight = InFlight::enter();
    if DRIVER.capturing() {
        shift_render_camera(level as usize, view as usize);
    }
    // SAFETY: forwards the engine's own call.
    unsafe { VIEW_SETUP_ORIGINAL.get()(level, view) }
}

/// The largest element difference (`camera::distance`) at which the render cache's camera is one
/// the view turned or the game set: the cache holds them as written, so a real match is near 0.
const CAMERA_MATCH: f32 = 0.01;

/// View setups by what the render cache held: [a turn of ours, the game's own camera, neither (a
/// camera the view did not see: another camera rendering)], and the largest distance of a match.
static CAMERA_MATCHES: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static CAMERA_MATCH_WORST: AtomicU32 = AtomicU32::new(0);

fn note_camera_match(known: bool, to_turned: f32, to_base: f32, eye: usize) {
    if !known {
        return;
    }
    let nearest = to_turned.min(to_base);
    let kind = if nearest >= CAMERA_MATCH {
        2
    } else {
        CAMERA_MATCH_WORST.fetch_max(nearest.to_bits(), Relaxed);
        if to_turned <= to_base { 0 } else { 1 }
    };
    let n = CAMERA_MATCHES[kind].fetch_add(1, Relaxed);
    if kind == 2 && (n < 6 || (n + 1).is_power_of_two()) {
        log!(
            "eye {eye}: the render cache holds a camera the view did not see ({} so far; {to_turned:.3} from the nearest turn, {to_base:.3} from the nearest game camera; cutscene {}): made from it as the game set it",
            n + 1,
            aim::cutscene()
        );
    }
}

/// Writes the eye camera (head pose, eye offset, the eye's projection) into the view's cached
/// render camera, which the setup and finish stages read.
fn shift_render_camera(level: usize, view: usize) {
    if !DRIVER.running() {
        return;
    }
    // This frame's eye, key and pose, as the present chose them together.
    let Some((pose, (chosen_at, eye))) = POSES.lock().ok().map(|p| (p.pending, p.chosen)) else { return };
    let setup = setup();
    let config = &setup.config;
    let Some(state) = mem::read::<usize>(view + engine::VIEW_RENDER_STATE).filter(|&s| s != 0) else {
        COUNTERS.shift_failures.fetch_add(1, Relaxed);
        return;
    };
    let Some(mut sample) = Camera::read(state) else {
        COUNTERS.shift_failures.fetch_add(1, Relaxed);
        return;
    };
    let mut shared = VIEW.lock().unwrap_or_else(|e| e.into_inner());
    if !DRIVER.capturing() {
        // Stopping: the end may already have given back what this would change again.
        return;
    }
    // The engine did not refresh the cache since our last write: offset the remembered centre
    // again instead of stacking a second offset.
    let stale = shared.last_center.is_some() && sample.inverse == shared.last_written;
    if let Some(center) = shared.last_center.filter(|_| stale) {
        sample = center;
    }
    crate::research::view::frame(eye, view, state, stale, &sample.inverse);
    IN_PLACE_VIEW.store(view, Relaxed);
    if let Some(frustum) = mem::read::<[f32; 5]>(state + engine::STATE_EXTENTS) {
        shared.frustum = frustum;
    }
    if config.headset_fov {
        relax_mesh_culling(&mut shared, level);
    }
    if config.no_raster_occlusion {
        let switch = view + engine::VIEW_NO_RASTER_OCCLUSION;
        if remember(&mut shared.occlusion, view, DRIVER.presents(), || mem::read::<u8>(switch)).is_some() {
            mem::write(switch, 1u8);
        }
    }
    let center = sample;
    // With the live camera turned, the cache is normally refreshed from it: the view centre of the
    // head pose it was turned with, which can be a present or two older than this frame's (with a
    // turning head, that jitters the eye). The view centre is made here from
    // the game's own camera every time: the turn's base when the cache holds a turn, else what the
    // game set (it rewrote the camera after the turn), with this frame's head pose.
    let mut turned = false;
    let mut base = (sample.inverse, aim::baked_yaw());
    if pose.valid != 0 && config.turn_live_camera
        && let Ok(poses) = POSES.lock()
    {
        let live = &poses.live;
        let known = live.history.min(TURNS);
        let to_base = (0..known).map(|i| distance(&sample.inverse, &live.bases[i])).fold(f32::INFINITY, f32::min);
        let to_turned = (0..known).map(|i| distance(&sample.inverse, &live.turned[i])).fold(f32::INFINITY, f32::min);
        // A turn of ours only when the cache really holds one: nearest alone would also pick a turn
        // for a camera that is neither (another camera rendering, as in a conversation), and that
        // eye would then be made from the player's camera.
        turned = known > 0 && to_turned <= to_base && to_turned < CAMERA_MATCH;
        note_camera_match(known > 0, to_turned, to_base, eye);
        if !turned {
            COUNTERS.head_reapplied.fetch_add(1, Relaxed);
        } else if let Some(i) = (0..known).min_by(|&a, &b| distance(&sample.inverse, &live.turned[a]).total_cmp(&distance(&sample.inverse, &live.turned[b]))) {
            note_turn_pair(eye, chosen_at & !1, live.pairs[i]);
            base = (live.bases[i], live.baked[i]);
        }
    }
    if pose.valid != 0 {
        // Head aim: the game camera carries the character's look, which already holds the aim
        // (head or hand, from the previous update). The view is that camera levelled, with the
        // aim's baked yaw turned back out, plus the current head pose.
        let steering = config.aim.head && aim::share_in_view();
        sample.inverse = view_center(&base.0, &pose, steering, steering && config.aim.hand, base.1);
        // The head's gap from the character: less what room-scale following has walked.
        crate::research::drift::note_head(pose.position, monaka_arms::roomscale::anchor());
        sample.view = rigid_inverse(&sample.inverse);
        COUNTERS.head_applied.fetch_add(1, Relaxed);
    }
    if !plausible_camera(&sample.inverse, &center.inverse) {
        note_implausible_camera("eye camera", &sample.inverse, &center.inverse);
        COUNTERS.shift_failures.fetch_add(1, Relaxed);
        return;
    }
    note_step(eye, &sample.inverse, turned);
    note_yaw(eye, &sample.inverse, turned);
    crate::research::view::cutscene(eye, chosen_at, turned, stale, &center.inverse, &base.0, base.1, &sample.inverse);
    let Some(eyes) = parallel_eyes(&sample.view, &sample.inverse, config.separation_for(&pose)) else {
        COUNTERS.shift_failures.fetch_add(1, Relaxed);
        return;
    };
    let desired = eyes[eye];
    crate::research::view::rig(eye, &sample.inverse, &desired, &pose, turned);
    shared.last_center = Some(center);
    shared.last_written = desired;
    drop(shared);

    // FSR: the frame's projection is shifted by its jitter (the cameras recorded below are not).
    let jitter = crate::output::upscale::jitter_for(chosen_at, eye);
    let mut jitter_written = [0.0; 2];
    if let Some(frustum) = config.rendered_frustum(&pose, eye)
        && let Some(near) = mem::read::<f32>(state + engine::STATE_NEAR).filter(|n| n.is_finite() && *n > 0.0)
    {
        let frustum = jitter.map_or(frustum, |j| crate::output::upscale::jittered(frustum, j));
        jitter_written = jitter.unwrap_or_default();
        // The engine derives the frustum from the near-plane extents; the matrix must agree.
        let mut projection = center.projection;
        frustum.apply_to_projection(&mut projection);
        engine::write_projection(state, &projection);
        mem::write(state + engine::STATE_EXTENTS, frustum.extents(near));
    }
    setup.camera.on_state(state, &desired);
    LAST_VIEW.store(DRIVER.presents(), Relaxed);
    RENDERED.store((chosen_at << 1) | eye as u64, Release);
    RENDER_THREAD.store(monaka_hook::thread_id(), Relaxed);
    if (crate::output::hybrid::enabled() || crate::output::upscale::enabled())
        && let Some(near) = mem::read::<f32>(state + engine::STATE_NEAR).filter(|n| n.is_finite() && *n > 0.0)
    {
        // Both eyes' cameras of this frame, for reprojecting it into the other eye, with the depth
        // terms the renderer's depth buffer really uses.
        let projection = |e: usize| {
            let mut p = center.projection;
            if let Some(frustum) = config.rendered_frustum(&pose, e) {
                frustum.apply_to_projection(&mut p);
            }
            engine::depth_buffer_projection(&p, near)
        };
        let projections = [projection(0), projection(1)];
        if let (Some(left), Some(right)) =
            (DepthMapping::from_projection(&projections[0], near), DepthMapping::from_projection(&projections[1], near))
        {
            let cameras = EyeCameras { camera: eyes, projection: projections, mapping: [left, right], pose };
            crate::output::hybrid::record_cameras(chosen_at, cameras);
            crate::output::upscale::record(chosen_at, cameras, jitter_written);
        }
    }
    if let Some(zoom) = config.detail_zoom {
        mem::write(state + engine::STATE_ZOOM, [zoom, zoom * zoom, 1.0 / zoom, 1.0 / (zoom * zoom)]);
    }
    COUNTERS.shifts.fetch_add(1, Relaxed);
}

/// `IBaseCamera::SetFOV`: on the player camera, asks for a field of view containing both eyes.
pub unsafe extern "system" fn set_fov(camera: *mut core::ffi::c_void, fov: f32) {
    let _flight = InFlight::enter();
    let mut fov = fov;
    if DRIVER.capturing() && DRIVER.running() && camera as usize == LIVE_INTERFACE.load(Relaxed) {
        let config = &setup().config;
        if config.headset_fov
            && config.widen_game_fov
            && let Some(needed) = Some(pending_pose()).filter(|p| p.valid != 0).as_ref().and_then(widened_fov)
            && needed > fov
        {
            fov = needed;
            COUNTERS.widened_fov_bits.store(fov.to_bits(), Relaxed);
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { SET_FOV_ORIGINAL.get()(camera, fov) }
}

/// Remembers the level's mesh size-cull limits once; they are then lowered every frame from the
/// present thread (a write from the view setup's thread does not survive to the next frame).
fn relax_mesh_culling(shared: &mut ViewState, level: usize) {
    if shared.culling_rejected == Some(level) {
        return;
    }
    let plausible = |mesh: f32, shadow: f32| (0.01..=200.0).contains(&mesh) && (0.01..=500.0).contains(&shadow);
    let read = || Some((engine::read_level_float(level, engine::MESH_CULL)?, engine::read_level_float(level, engine::SHADOW_MESH_CULL)?)).filter(|&(m, s)| plausible(m, s));
    if remember(&mut shared.culling, level, DRIVER.presents(), read).is_none() {
        shared.culling_rejected = Some(level);
        log!("mesh culling left alone for level {level:#x}: its variables did not read as plausible limits");
    }
}

/// Every present: the relaxed limits again into each level rendered just now (the engine
/// overwrites the store the setters write from its other buffer every frame).
fn apply_mesh_culling(scale: f32) {
    let now = DRIVER.presents();
    let Ok(shared) = VIEW.lock() else { return };
    for c in shared.culling.iter().filter(|c| c.rendered_within(ACTIVE_PRESENTS, now)) {
        let (mesh, shadow) = c.saved;
        engine::write_level_float(c.object, engine::MESH_CULL, mesh * scale);
        engine::write_level_float(c.object, engine::SHADOW_MESH_CULL, shadow * scale);
    }
}

/// Gives back everything the run changed, once, from whichever thread retires the eye (the
/// driver's finish).
fn finish(driver: &Driver) {
    log!("presents by how far back the camera they show was chosen [0, 1, 2, 3, 4, 5+]: {:?}", LABEL_LAGS.each_ref().map(|c| c.load(Relaxed)));
    log!(
        "view centre steps back while moving, by eye [made like the frame before, made the other way] of frames moving: left {:?} of {}, right {:?} of {}",
        REVERSALS[0].each_ref().map(|c| c.load(Relaxed)),
        MOVING[0].load(Relaxed),
        REVERSALS[1].each_ref().map(|c| c.load(Relaxed)),
        MOVING[1].load(Relaxed)
    );
    log!(
        "view yaw jumps over half a degree, by eye [turned live camera, rebuilt at view setup]: left {:?} of {}, right {:?} of {}",
        YAW_JUMPS[0].each_ref().map(|c| c.load(Relaxed)),
        YAW_FRAMES[0].load(Relaxed),
        YAW_JUMPS[1].each_ref().map(|c| c.load(Relaxed)),
        YAW_FRAMES[1].load(Relaxed)
    );
    log!("eye cameras from a turned live camera [this pair's head, an older one]: left {:?}, right {:?}", TURN_PAIRS[0].each_ref().map(|c| c.load(Relaxed)), TURN_PAIRS[1].each_ref().map(|c| c.load(Relaxed)));
    log!(
        "render cache cameras [a turn of ours, the game's own, neither]: {:?}; the farthest match {:.5}",
        CAMERA_MATCHES.each_ref().map(|c| c.load(Relaxed)),
        f32::from_bits(CAMERA_MATCH_WORST.load(Relaxed))
    );
    let setup = setup();
    {
        let now = DRIVER.presents();
        let mut shared = VIEW.lock().unwrap_or_else(|e| e.into_inner());
        let mut gone = 0;
        for c in std::mem::take(&mut shared.occlusion) {
            match c.rendered_within(RESTORE_PRESENTS, now) {
                true => _ = mem::write(c.object + engine::VIEW_NO_RASTER_OCCLUSION, c.saved),
                false => gone += 1,
            }
        }
        for c in std::mem::take(&mut shared.culling) {
            if c.rendered_within(RESTORE_PRESENTS, now) {
                engine::write_level_float(c.object, engine::MESH_CULL, c.saved.0);
                engine::write_level_float(c.object, engine::SHADOW_MESH_CULL, c.saved.1);
            } else {
                gone += 1;
            }
        }
        if gone > 0 {
            log!("{gone} changed levels or views were no longer rendered at the end; left alone");
        }
        shared.last_center = None;
    }
    // Give the player camera back as the game left it (called outside our lock: it is engine code).
    let restore = POSES.lock().ok().and_then(|mut poses| {
        let live = &mut poses.live;
        live.history = 0;
        live.base.take().filter(|_| live.interface != 0).map(|base| (live.interface, base))
    });
    // Still a camera interface whose state points back at it (a level change frees both).
    let alive = |interface: usize| {
        mem::read::<usize>(interface + engine::INTERFACE_STATE).filter(|&s| s != 0).and_then(|state| mem::read::<usize>(state + engine::STATE_INTERFACE)) == Some(interface)
    };
    match restore {
        Some((interface, base)) if alive(interface) => setup.camera.on_interface(interface, &base),
        Some(_) => log!("the player camera was gone at the end; not given back"),
        None => {}
    }
    LIVE_INTERFACE.store(0, Relaxed);
    aim::release();
    aim::report();
    crate::player::hands::report();
    crate::research::finish();
    crate::hud::panels::report();
    crate::output::hybrid::report();
    crate::output::upscale::report();
    let (pairs, failures) = driver.with_publisher11(|p| (p.pairs(), p.failures())).unwrap_or((0, 0));
    let c = &COUNTERS;
    log!(
        "run ended: pairs={pairs} publish_failures={failures} camera_shifts={} shift_failures={} head_applied={} head_reapplied={} live_turns={} widened_fov={} hud_draws_shifted={} draws_seen={} scene_copies={} hud_layer_draws={}",
        c.shifts.load(Relaxed),
        c.shift_failures.load(Relaxed),
        c.head_applied.load(Relaxed),
        c.head_reapplied.load(Relaxed),
        c.live_turns.load(Relaxed),
        f32::from_bits(c.widened_fov_bits.load(Relaxed)),
        draws::shifted_draws(),
        draws::draws_seen(),
        draws::scene_copies(),
        draws::layer_draws(),
    );
}

/// Releases the channel and the pose block after the hooks are off.
pub fn release() {
    DRIVER.release();
    HANDS.lock().unwrap_or_else(|e| e.into_inner()).0 = None;
    crate::output::hybrid::release();
    crate::research::release();
    crate::output::upscale::release();
}
