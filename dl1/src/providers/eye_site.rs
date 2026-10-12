//! Provider: the eye site (`monaka_stereo::eye_site::EyeSite`: reading the game camera and writing
//! an eye into it, installed with the present loop), on the shared recipe
//! (`monaka_core::camera::EyeView::of`).
//!
//! DL1's eye site, the provider side of alternate-eye stereo in place: what only Dying Light 1
//! knows about its cameras. The player camera stays on the game's view. Monaka's present loop
//! (`monaka_stereo::feature`, its `DRIVER`) chooses the next eye and head pose after each present
//! and hands them here ([`choose`]); the view setup stage moves the cached render camera to that
//! eye just before the renderer reads it ([`view_setup`]); the live player camera is turned to the
//! view as the game sets it and at each present ([`turn_after_game_update`], [`turn_at_present`]);
//! the game is asked for a field of view containing both eyes ([`set_fov`]); the levels' cull limits
//! and the views' occlusion switch are changed for the headset's view and given back at the end
//! ([`give_back`]). The frame then shows `latency` presents later.
//!
//! DL1's steps around the present loop are its present entry's (`crate::providers::present_entry`); the site's
//! measurements are Monaka's (`monaka_stereo::eye_site::checks`).

use monaka_arms::owned_facing::FACING;
use crate::config::{Config, widened_fov};
use crate::providers::engine::{self, Camera, CameraWriter, SetFovFn, SetInverseFn, ViewStageFn};
use crate::Dl1;
use monaka_core::alternate::Schedule;
use monaka_core::camera::{self, Base, EyeView, Frustum, Mat34, distance, eye_camera};
use monaka_core::protocol::HeadPose;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use monaka_core::depth::DepthMapping;
use monaka_stereo::eye_site::{EyeSite, Tally, checks};
use monaka_stereo::feature::DRIVER;
use monaka_stereo::afw11::EyeCameras;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// The eye site as the present loop reads it, installed with it: the view setup hooked (and SetFOV
/// with the headset's field of view widened), the controllers' block for hand aim opened. Each pair
/// goes out with the frustum each eye was rendered with: the engine's own without the headset's
/// projection, the centred envelope with the symmetric experiment, else the headset's as the pose
/// carries it.
impl EyeSite for Dl1 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        install_eyes(start)
    }

    fn tally() -> &'static Tally {
        &VIEWS
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        drawn_frustum(&SETUP.get()?.config, game_frustum(), pose, eye)
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        separation(pose)
    }
}

/// The frustum eye `eye` of `pose` was drawn with where it is not the pose's own, for `config`: the
/// engine's own (`game`) without the headset's projection, the centred envelope with the symmetric
/// experiment.
fn drawn_frustum(config: &Config, game: Option<Frustum>, pose: &HeadPose, eye: usize) -> Option<Frustum> {
    if !config.headset_fov {
        game
    } else if config.symmetric_projection {
        config.rendered_frustum(pose, eye)
    } else {
        None
    }
}

fn install_eyes(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let engine_module = &start.build.engine;
    let set_inverse = engine_module.export(engine::SET_INVERSE_CAMERA).ok_or_else(|| Rejection::revision("the engine does not export SetInvCameraMatrix"))?;
    if let Some(missing) = engine::MESH_CULL_SETTERS.iter().find(|name| engine_module.export(name).is_none()) {
        return Err(Rejection::revision(format!("the engine does not export {missing}")).into());
    }
    let channel = start.channel()?;
    // Hand aim reads the controllers the viewer writes beside the pose (sampled with each frame,
    // `monaka_arms::hands`); without them, head aim.
    if config.aim.hand {
        monaka_arms::controllers::open(channel);
    }
    // SAFETY: the export is `void IBaseCamera::SetInvCameraMatrix(const mtx34&)` on x64.
    let camera = CameraWriter(unsafe { std::mem::transmute::<usize, SetInverseFn>(set_inverse) });
    install(Setup { config, camera });
    start.hooks.site(&VIEW_SETUP_ORIGINAL, engine_module, &engine::VIEW_SETUP, view_setup)?;
    // The field-of-view widening serves both the live-camera turn and head aim.
    if config.headset_fov && config.widen_game_fov {
        start.hooks.site(&SET_FOV_ORIGINAL, engine_module, &engine::SET_FOV, set_fov)?;
    }
    Ok(())
}

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
}

static SETUP: OnceLock<Setup> = OnceLock::new();

/// The run's setup.
pub fn install(setup: Setup) {
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

/// The eyes' separation for `pose`, once installed.
pub fn separation(pose: &HeadPose) -> Option<f32> {
    Some(SETUP.get()?.config.separation_for(pose))
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
    Mutex::new(Poses { pending: HeadPose::EMPTY, chosen: (0, 0), live: LiveCamera { interface: 0, base: None, bases: [[0.0; 12]; TURNS], turned: [[0.0; 12]; TURNS], pairs: [0; TURNS], baked: [0.0; TURNS], history: 0 } });
static LIVE_INTERFACE: AtomicUsize = AtomicUsize::new(0);

/// After present `n`: the next frame's eye is chosen with the pair's head `pose`; the next view
/// setup draws from it.
pub(crate) fn choose(n: u64, pose: HeadPose) {
    if let Ok(mut poses) = POSES.lock() {
        poses.pending = pose;
        poses.chosen = (n, Schedule::camera_eye(n));
    }
}

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

/// The engine's own frustum, as the latest view setup read it from the render state (the eyes are
/// drawn with it without the headset's projection).
pub fn game_frustum() -> Option<Frustum> {
    let frustum = VIEW.lock().map(|v| v.frustum).unwrap_or_default();
    Frustum::from_extents([frustum[0], frustum[1], frustum[2], frustum[3]], frustum[4])
}

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

/// View setups while stereo runs, and the render cameras moved to an eye at them (the loop's rate
/// line), refused, and made on a camera carrying head aim.
pub static VIEWS: Tally = Tally::new("view setups");

struct Counters {
    head_applied: AtomicU64,
    head_reapplied: AtomicU64,
    live_turns: AtomicU64,
    widened_fov_bits: AtomicU32,
}
static COUNTERS: Counters = Counters {
    head_applied: AtomicU64::new(0),
    head_reapplied: AtomicU64::new(0),
    live_turns: AtomicU64::new(0),
    widened_fov_bits: AtomicU32::new(0),
};

/// The site's counts for the end of a run: eye cameras made with the head pose, those made from
/// the game's camera again (not from a turn of ours), live-camera turns, and the widest field of
/// view asked for.
pub(crate) struct Counts {
    pub head_applied: u64,
    pub head_reapplied: u64,
    pub live_turns: u64,
    pub widened_fov: f32,
}

pub(crate) fn counts() -> Counts {
    let c = &COUNTERS;
    Counts {
        head_applied: c.head_applied.load(Relaxed),
        head_reapplied: c.head_reapplied.load(Relaxed),
        live_turns: c.live_turns.load(Relaxed),
        widened_fov: f32::from_bits(c.widened_fov_bits.load(Relaxed)),
    }
}

pub static VIEW_SETUP_ORIGINAL: Original<ViewStageFn> = Original::new();
pub static SET_FOV_ORIGINAL: Original<SetFovFn> = Original::new();

/// After present `n`, with the pair's head `pose`: the live camera turned to the view. Climbing,
/// the game's camera is left as the game set it: hanging, the game turns the character by where
/// its camera looks ([`monaka_arms::traverse`]). With no turn on record the view centre is made
/// from the game's camera at render.
pub(crate) fn turn_at_present(pose: &HeadPose, n: u64) {
    let setup = setup();
    if setup.config.turn_live_camera && pose.valid != 0 {
        if monaka_arms::traverse::traversing() {
            if let Ok(mut poses) = POSES.lock() {
                poses.live.history = 0;
                poses.live.base = None;
            }
        } else {
            let steering = setup.config.aim.head && FACING.share_in_view();
            turn_live_camera(&setup.camera, pose, n & !1, steering, steering && setup.config.aim.hand);
        }
    }
}

/// How far the view backs off the wall now (metres) and which way (a horizontal world direction):
/// `climb_pullback` away from where the hands hold on ([`monaka_arms::traverse::wall_direction`]),
/// eased with the arms' blend into the climb.
fn climb_pullback() -> Option<(f32, [f32; 2])> {
    let distance = config().climb_pullback * monaka_arms::traverse::climb_share();
    let wall = monaka_arms::traverse::wall_direction()?;
    (distance > 1e-4).then(|| (distance, [-wall[0], -wall[1]]))
}

/// How the view is built on the game camera `game`. The view's facing is the mod's: while head aim
/// steers, `game` carries the aim (head or hand), so it is levelled with the aim's yaw `baked`
/// ([`FACING`]'s baked yaw as the game built it) turned back out; otherwise (or for a camera looking
/// straight up or down) it is taken as the game set it.
fn view_base(game: &Mat34, head_aim: bool, baked: f32) -> Base {
    match head_aim && camera::level(game).is_some() {
        true => Base::Level { baked_yaw: baked },
        false => Base::Game,
    }
}

/// The game camera `game` where the view is built on it ([`view_base`]): with hand aim
/// (`unlean`) the game leans its camera with the controller's pitch (0.37 m back at 60 degrees up),
/// and that lean is taken back out, so the eyes stay put and only the tracked head moves them;
/// climbing, it is backed off the wall to see the climb (`pullback`, [`climb_pullback`]).
fn placed(game: &Mat34, unlean: bool, pullback: Option<(f32, [f32; 2])>) -> Mat34 {
    let mut game = *game;
    if unlean {
        // Camera-to-world: column 2 is the back axis, column 3 the position.
        let pitch = (-game[6]).clamp(-1.0, 1.0).asin().to_degrees();
        let (forward, up) = engine::camera_lean(pitch);
        let (x, z) = (-game[2], -game[10]);
        let length = (x * x + z * z).sqrt();
        if length > 1e-4 {
            game[3] -= x / length * forward;
            game[11] -= z / length * forward;
            game[7] -= up;
        }
    }
    if let Some((distance, [x, z])) = pullback {
        game[3] += x * distance;
        game[11] += z * distance;
    }
    game
}

/// The game camera `game` the view for the head `pose` is built on as `base` ([`placed`]), moved
/// back by room-scale following: the head's movement the character has walked is in the camera
/// now ([`monaka_arms::roomscale::follow_view`], the head where the head model draws it).
fn view_game(game: &Mat34, base: Base, unlean: bool, pose: &HeadPose) -> Option<Mat34> {
    monaka_arms::duck::note_head(pose.position[1]);
    monaka_arms::roomscale::follow_view(&placed(game, unlean, climb_pullback()), base, config().head_position.position(pose))
}

/// The view centre (between the eyes) for the game camera `game` and head `pose`, as the eye site
/// makes it ([`shift_render_camera`]): `game` taken as [`view_base`] says, the head placed by the
/// head model (`head_position`).
fn view_center(game: &Mat34, pose: &HeadPose, head_aim: bool, unlean: bool, baked: f32) -> Option<Mat34> {
    let base = view_base(game, head_aim, baked);
    camera::view_centre(&view_game(game, base, unlean, pose)?, base, pose.orientation, config().head_position.position(pose))
}

/// Where the headset's tracking space sits in the world for the game camera `game` (see
/// [`view_center`]), moved as the head model draws the pending head: any tracked pose, head or
/// controller, lands in the world through it where the eyes see it.
pub fn tracking_origin(game: &Mat34, head_aim: bool, unlean: bool) -> Mat34 {
    let placed = placed(game, unlean, climb_pullback());
    let mut origin = view_base(game, head_aim, FACING.baked_yaw()).origin(&placed).unwrap_or(placed);
    monaka_arms::roomscale::shift_origin(&mut origin);
    config().head_position.drawn_origin(&origin, &pending_pose())
}

/// A camera the view would write is finite and within this of the game's own (metres): the head,
/// the climb pullback and the eyes move it well under that. The game looks things up near its
/// camera, and a position not finite or far out crashes it.
const CAMERA_REACH: f32 = 5.0;

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
    let steering = setup.config.aim.head && FACING.share_in_view();
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
    let baked = FACING.baked_yaw();
    let Some(turned) = view_center(&base, pose, head_aim, unlean, baked) else { return };
    if !camera::within_reach(&turned, &base, CAMERA_REACH) {
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
pub(crate) const CAMERA_MATCH: f32 = 0.01;

/// Writes the eye camera (head pose, eye offset, the eye's projection) into the view's cached
/// render camera, which the setup and finish stages read.
fn shift_render_camera(level: usize, view: usize) {
    if !DRIVER.running() {
        return;
    }
    let call = VIEWS.call();
    // This frame's eye, key and pose, as the present chose them together.
    let Some((pose, (chosen_at, eye))) = POSES.lock().ok().map(|p| (p.pending, p.chosen)) else { return };
    let setup = setup();
    let config = &setup.config;
    let Some(state) = mem::read::<usize>(view + engine::VIEW_RENDER_STATE).filter(|&s| s != 0) else {
        VIEWS.refused(call, "the view has no render state");
        return;
    };
    let Some(mut sample) = Camera::read(state) else {
        VIEWS.refused(call, "the render camera is not readable");
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
    let mut base = (sample.inverse, FACING.baked_yaw());
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
        checks::note_camera_match(known > 0, to_turned, to_base, CAMERA_MATCH, eye, FACING.cutscene());
        if !turned {
            COUNTERS.head_reapplied.fetch_add(1, Relaxed);
        } else if let Some(i) = (0..known).min_by(|&a, &b| distance(&sample.inverse, &live.turned[a]).total_cmp(&distance(&sample.inverse, &live.turned[b]))) {
            checks::note_turn_pair(eye, chosen_at & !1, live.pairs[i]);
            base = (live.bases[i], live.baked[i]);
        }
    }
    // Head aim: the game camera carries the character's look, which already holds the aim (head or
    // hand, from the previous update). The view is that camera levelled, with the aim's baked yaw
    // turned back out, plus the current head pose as the head model places it. Without a head
    // pose, the eyes straddle the cache's camera.
    let steering = config.aim.head && FACING.share_in_view();
    let taken = view_base(&base.0, steering, base.1);
    let game = match pose.valid != 0 {
        true => view_game(&base.0, taken, steering && config.aim.hand, &pose),
        false => Some(sample.inverse),
    };
    let game_fov = Frustum::from_extents([shared.frustum[0], shared.frustum[1], shared.frustum[2], shared.frustum[3]], shared.frustum[4])
        .map_or([0.0; 2], |f| monaka_core::options::full_angles(&f));
    let Some(drawn) = game.and_then(|game| EyeView::of(&game, taken, &pose, eye, config, config.symmetric_projection, game_fov)) else {
        VIEWS.refused(call, format_args!("eye {eye}: game camera {:?}, field of view {game_fov:?}", base.0));
        return;
    };
    if pose.valid != 0 {
        // The head's gap from the character: less what room-scale following has walked.
        crate::research::drift::note_head(pose.position, monaka_arms::roomscale::anchor());
        COUNTERS.head_applied.fetch_add(1, Relaxed);
        if steering {
            VIEWS.aimed();
        }
    }
    if !camera::within_reach(&drawn.centre, &center.inverse, CAMERA_REACH) {
        VIEWS.refused(call, format_args!("eye {eye}: {:?} implausibly far from the game's {:?}", drawn.centre, center.inverse));
        return;
    }
    checks::note_step(eye, &drawn.centre, turned);
    checks::note_yaw(eye, &drawn.centre, turned);
    crate::research::view::cutscene(eye, chosen_at, turned, stale, &center.inverse, &base.0, base.1, &drawn.centre);
    let separation = config.separation_for(&pose);
    let eyes = [0, 1].map(|e| eye_camera(&drawn.centre, e, separation));
    let desired = drawn.camera;
    crate::research::view::rig(eye, &drawn.centre, &desired, &pose, turned);
    shared.last_center = Some(center);
    shared.last_written = desired;
    drop(shared);

    // FSR: the frame's projection is shifted by its jitter (the cameras recorded below are not).
    // The headset's frustum, while the headset's field of view is on and a head is tracked.
    let jitter = monaka_stereo::upscaler::fsr11::jitter_for(chosen_at, eye);
    let mut jitter_written = [0.0; 2];
    if let Some(frustum) = (config.headset_fov && pose.valid != 0).then_some(drawn.frustum)
        && let Some(near) = mem::read::<f32>(state + engine::STATE_NEAR).filter(|n| n.is_finite() && *n > 0.0)
    {
        let frustum = jitter.map_or(frustum, |j| monaka_stereo::upscaler::fsr11::jittered(frustum, j));
        jitter_written = jitter.unwrap_or_default();
        // The engine derives the frustum from the near-plane extents; the matrix must agree.
        let mut projection = center.projection;
        frustum.apply_to_projection(&mut projection);
        engine::write_projection(state, &projection);
        mem::write(state + engine::STATE_EXTENTS, frustum.extents(near));
    }
    setup.camera.on_state(state, &desired);
    checks::rendered(chosen_at, eye);
    if (monaka_stereo::afw11::ready::enabled() || monaka_stereo::upscaler::fsr11::enabled())
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
            monaka_stereo::afw11::ready::record_cameras(chosen_at, cameras);
            monaka_stereo::upscaler::fsr11::record(chosen_at, cameras, jitter_written);
        }
    }
    if let Some(zoom) = config.detail_zoom {
        mem::write(state + engine::STATE_ZOOM, [zoom, zoom * zoom, 1.0 / zoom, 1.0 / (zoom * zoom)]);
    }
    VIEWS.drawn();
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
pub(crate) fn apply_mesh_culling(scale: f32) {
    let now = DRIVER.presents();
    let Ok(shared) = VIEW.lock() else { return };
    for c in shared.culling.iter().filter(|c| c.rendered_within(ACTIVE_PRESENTS, now)) {
        let (mesh, shadow) = c.saved;
        engine::write_level_float(c.object, engine::MESH_CULL, mesh * scale);
        engine::write_level_float(c.object, engine::SHADOW_MESH_CULL, shadow * scale);
    }
}

/// Gives back what the run changed in the game, once, from whichever thread retires the eye (the
/// driver's finish): the views' occlusion switches, the levels' cull limits and the player camera.
pub(crate) fn give_back() {
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
}

#[cfg(test)]
mod lift_checks;

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::camera::{HeadPosition, apply_head, from_back_up, view_centre};
    use monaka_core::math::{NECK_TO_EYES, rotate};

    /// A game camera looking 30 degrees left and 25 up (hand aim pitches it), at (5, 1, 2).
    fn game() -> Mat34 {
        let (yaw, pitch) = (30f32.to_radians(), 25f32.to_radians());
        from_back_up([yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()], [0.0, 1.0, 0.0], [5.0, 1.0, 2.0]).unwrap()
    }

    /// The view centre as it was made before: the lean taken out, levelled with the baked yaw
    /// turned out under head aim, backed off the wall, the tracked head on that.
    fn centre_before(game: &Mat34, pose: &HeadPose, head_aim: bool, unlean: bool, baked: f32, pullback: Option<(f32, [f32; 2])>) -> Mat34 {
        let mut base = *game;
        if unlean {
            let pitch = (-base[6]).clamp(-1.0, 1.0).asin().to_degrees();
            let (forward, up) = engine::camera_lean(pitch);
            let (x, z) = (-base[2], -base[10]);
            let length = (x * x + z * z).sqrt();
            base[3] -= x / length * forward;
            base[11] -= z / length * forward;
            base[7] -= up;
        }
        let mut origin = head_aim.then(|| camera::tracking_origin(&base, baked)).flatten().unwrap_or(base);
        if let Some((distance, [x, z])) = pullback {
            origin[3] += x * distance;
            origin[11] += z * distance;
        }
        apply_head(&origin, pose.orientation, pose.position)
    }

    #[test]
    fn the_full_head_gives_the_view_centre_it_gave_before() {
        // The head turned 0.6 rad left and 0.2 up, stepped forward and to the right.
        let ((sy, cy), (sp, cp)) = (0.3f32.sin_cos(), 0.1f32.sin_cos());
        let pose = HeadPose { valid: 1, orientation: [cy * sp, sy * cp, -sy * sp, cy * cp], position: [0.1, 1.6, -0.2], ..HeadPose::default() };
        let game = game();
        for (head_aim, unlean) in [(false, false), (true, false), (true, true)] {
            for pullback in [None, Some((0.35, [0.6, -0.8]))] {
                let base = view_base(&game, head_aim, 0.3);
                let after = view_centre(&placed(&game, unlean, pullback), base, pose.orientation, HeadPosition::Full.position(&pose)).unwrap();
                let before = centre_before(&game, &pose, head_aim, unlean, 0.3, pullback);
                assert!(distance(&after, &before) < 1e-5, "head aim {head_aim}, unlean {unlean}, {pullback:?}: {after:?} {before:?}");
            }
        }
        // Rolled onto its side: no yaw to level by, so the camera as the game set it.
        let rolled = from_back_up([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0; 3]).unwrap();
        assert_eq!(view_base(&rolled, true, 0.3), Base::Game);
    }

    #[test]
    fn the_default_head_turns_about_the_neck_where_the_view_stands() {
        // The head turned 40 degrees left about the neck: tracked, the eyes swing left and back;
        // the lean head model keeps the view centre where the levelled camera stands.
        let (s, c) = 20f32.to_radians().sin_cos();
        let orientation = [0.0, s, 0.0, c];
        let swing = rotate(orientation, NECK_TO_EYES);
        let pose = HeadPose { valid: 1, orientation, position: [0, 1, 2].map(|k| swing[k] - NECK_TO_EYES[k]), ..HeadPose::default() };
        let game = game();
        let base = view_base(&game, true, 0.0);
        let lean = view_centre(&game, base, orientation, HeadPosition::Lean.position(&pose)).unwrap();
        assert!((0..3).all(|k| (lean[k * 4 + 3] - game[k * 4 + 3]).abs() < 1e-5), "{lean:?}");
        let full = view_centre(&game, base, orientation, HeadPosition::Full.position(&pose)).unwrap();
        assert!((0..3).map(|k| (full[k * 4 + 3] - game[k * 4 + 3]).powi(2)).sum::<f32>().sqrt() > 0.02, "full: the eyes swing with the neck");
    }
}
