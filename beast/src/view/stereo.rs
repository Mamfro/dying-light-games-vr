//! Alternate-eye stereo on D3D12, on Monaka's present loop (`monaka_stereo::feature`, its
//! `DRIVER`, entered from the present request: `output::present12`): after each present the loop
//! chooses the next eye; the game's player camera update takes the choices in turn and sets the
//! camera to that eye ([`eye_vectors`], in the `SetView(vec3)` detour), and the renderer labels
//! each frame with its eye when DLSS gets its camera (`output::streamline`), so the frame goes out
//! as that eye with its pose, whatever the timing.
//!
//! The camera written is the game's live player camera (there is no separate render copy to move,
//! as in DL1), so culling, shadows and the renderer's own camera copies all follow the eye. The
//! game rebuilds the camera from the character's look angles every update, so nothing stacks.
//! Each eye is rendered with the headset's field of view, centred ([`before_frustum`]), or the
//! game's; its pose goes out with the one it was rendered with ([`report`]).

use crate::engine;
use monaka_channel::ChannelName;
use monaka_core::camera::{self, Frustum, Mat34};
use monaka_core::hud::{Placement, Rect};
use monaka_core::protocol::HeadPose;
use monaka_core::sync::Locked;
use monaka_hook::{AtomicF32, mem};
use monaka_producer::log;
use monaka_stereo::driver::Driver;
use monaka_stereo::feature::DRIVER;
use std::sync::Mutex;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize};
use windows::Win32::Graphics::Dxgi::{IDXGISwapChain, IDXGISwapChain3};
use windows::core::Interface;

/// The eye the last player camera update was given (-1: none), and the pose it was drawn from.
static CURRENT_EYE: AtomicI32 = AtomicI32::new(-1);
static PENDING: Mutex<HeadPose> = Mutex::new(HeadPose::EMPTY);
/// Eye separation (metres) when the headset reports no plausible IPD.
static SEPARATION: AtomicF32 = AtomicF32::new(0.064);
/// PC check of the eye order: eye 0 turned this far left, eye 1 as far right (radians).
static EYE_TEST: AtomicF32 = AtomicF32::zero();
/// The game camera's projection scales P00 and P11 (its field of view), from the last update.
static GAME_SCALE: [AtomicF32; 2] = [AtomicF32::zero(), AtomicF32::zero()];
/// PC check of the pose records' timing: the head of every other pair turned this far (radians).
static LATENCY_TEST: AtomicF32 = AtomicF32::zero();

/// The HUD's size and distance while it is laid over the eyes ([`Settings::hud`]).
static HUD: Mutex<Option<(f32, f32)>> = Mutex::new(None);

/// Each eye rendered with the headset's field of view (centred, see [`Frustum::symmetric`]): the
/// player camera's near-plane extents are written just before the game rebuilds its projection
/// from them, and the game's own extents are put back the same way at the end.
static HEADSET_FOV: AtomicBool = AtomicBool::new(false);
static PLAYER_CAMERA: AtomicUsize = AtomicUsize::new(0);
static FOV: Mutex<FovState> = Mutex::new(FovState { game: None, written: [0.0; 4], restore: false, restored: false });

struct FovState {
    /// The game's own extents, from before the first write.
    game: Option<[f32; 4]>,
    written: [f32; 4],
    /// Put the game's extents back at the next rebuild (stop).
    restore: bool,
    restored: bool,
}

/// Eye cameras written (the loop's rate line), refused, frames held for the eye size, headset
/// frusta written.
pub static EYE_WRITES: AtomicU64 = AtomicU64::new(0);
static EYE_REFUSALS: AtomicU64 = AtomicU64::new(0);
static SIZE_WAITS: AtomicU64 = AtomicU64::new(0);
static FRUSTA: AtomicU64 = AtomicU64::new(0);

pub struct Settings {
    pub channel: ChannelName,
    pub separation: f32,
    pub eye_test_degrees: f32,
    pub latency_test_degrees: f32,
    pub headset_fov: bool,
    /// The HUD's size (1 its size on a monitor) and distance (metres) in front of the eyes, when
    /// it is laid over them (`eng_chr::hudover`).
    pub hud: Option<(f32, f32)>,
}

/// The stereo channel's name, once stereo started (the dynamic HUD's panel channels sit beside it).
static CHANNEL: Mutex<Option<ChannelName>> = Mutex::new(None);

pub fn channel() -> Option<ChannelName> {
    CHANNEL.lock().ok().and_then(|c| c.clone())
}

/// The run's eye settings (the loop itself is the present component's).
pub fn install(settings: Settings) {
    *CHANNEL.locked() = Some(settings.channel.clone());
    SEPARATION.store(settings.separation);
    EYE_TEST.store(settings.eye_test_degrees.to_radians());
    LATENCY_TEST.store(settings.latency_test_degrees.to_radians());
    *FOV.locked() = FovState { game: None, written: [0.0; 4], restore: false, restored: false };
    HEADSET_FOV.store(settings.headset_fov, Release);
    *HUD.locked() = settings.hud;
    let test = |name: &str, degrees: f32| if degrees != 0.0 { format!(", {name} test {degrees} degrees") } else { String::new() };
    log!(
        "alternate-eye stereo (eye separation {} m, {} field of view{}{})",
        settings.separation,
        if settings.headset_fov { "headset" } else { "game" },
        test("eye", settings.eye_test_degrees),
        test("latency", settings.latency_test_degrees),
    );
}

/// Every hook is in (and the eye size asked for): stereo starts with the next present.
pub fn arm() {
    DRIVER.start();
}

/// Stops writing eye cameras (the next update is the game's own again) and has the next projection
/// rebuild put the game's extents back. Hooks must still be in; returns once the extents are back
/// (or after `wait_ms`, logged).
pub fn quiet(wait_ms: u64) {
    DRIVER.retire(wait_ms);
    CURRENT_EYE.store(-1, Release);
    let pending = {
        let mut fov = FOV.locked();
        fov.restore = fov.game.is_some();
        fov.restore
    };
    if pending && !monaka_producer::wait_until(wait_ms, || FOV.lock().map(|f| f.restored).unwrap_or(true)) {
        log!("the game's field of view was not put back within {wait_ms} ms (the game is paused?); it returns at its next field of view change");
    }
    HEADSET_FOV.store(false, Release);
}

pub fn release() {
    CHANNEL.locked().take();
}

/// Whether stereo runs (eye cameras are written).
pub fn running() -> bool {
    DRIVER.running()
}

/// The eye of the frame the renderer is working on now (the one the next present shows), while
/// stereo runs.
pub fn rendering_eye() -> Option<usize> {
    DRIVER.schedule()?.shown_eye(DRIVER.drawing_present()?)
}

/// Before the loop publishes present `n`: held while the game is not yet at the headset's eye
/// size (the channel takes the first published frame's size); with frame generation, its own
/// pairs from the labelled frames. True when the loop is to publish nothing itself.
pub fn before_present(driver: &Driver, chain: &IDXGISwapChain, n: u64) -> bool {
    if !crate::output::video::ready() {
        driver.take_label();
        SIZE_WAITS.fetch_add(1, Relaxed);
        return true;
    }
    let label = driver.peek_label();
    if let Some(shown) = label.map(|l| l.eye).or_else(|| driver.schedule().and_then(|s| s.shown_eye(n))) {
        crate::research::history::shown(shown);
    }
    if !crate::output::framegen::enabled() {
        return false;
    }
    // Frame generation makes and publishes the pairs itself, from labelled frames (an unlabelled
    // frame, a menu or the pause screen, flat to both eyes).
    let (Ok(swap), Some(queue)) = (chain.cast::<IDXGISwapChain3>(), crate::output::present12::game_queue()) else { return true };
    let report = |pose: &mut HeadPose| report(driver, pose);
    match driver.take_label() {
        Some(label) => crate::output::framegen::present(&swap, &queue, label.eye, label.pose, driver.head(), report),
        None => crate::output::framegen::present_unlabelled(&swap, &queue, driver.head(), report),
    };
    true
}

/// After present `n`: with the latency check, which head each pair's record says (to match against
/// the probe's saved pairs).
pub fn after_present(driver: &Driver, _: u64, _: &HeadPose) {
    if LATENCY_TEST.load() != 0.0
        && let Some((sequence, eyes)) = driver.last_pair()
    {
        monaka_producer::log_first!(600, "pair {sequence}: record head yaw {:.1} degrees", monaka_core::math::yaw(eyes[0].orientation).to_degrees());
    }
}

/// The latency check: every other pair's head turned left, so its images must show the turn its
/// record says.
pub fn latency_test(n: u64, pose: &mut HeadPose) {
    let test = LATENCY_TEST.load();
    if test != 0.0 && pose.valid != 0 && (n / 2).is_multiple_of(2) {
        pose.orientation = monaka_core::math::yaw_rotation(test);
    }
}

/// Each eye goes out with the field of view it was rendered with: its own centred, or the game's.
pub fn report(_: &Driver, used: &mut HeadPose) {
    used.ipd = used.ipd_or(SEPARATION.load());
    let pose = *used;
    for (eye, fov) in used.fov.iter_mut().enumerate() {
        if let Some(frustum) = eye_frustum(&pose, eye) {
            *fov = frustum.fov();
        }
    }
}

/// How the HUD layer goes over the frame about to present (`size` its pixels): in its eye's place
/// at the HUD's distance and size, with that eye's disparity; a frame no eye camera drew (a menu
/// over a still world) gets it centred, with none. `None` while stereo does not run (the game's own
/// layout then).
pub fn hud_over(size: (u32, u32)) -> Option<eng_chr::hudover::Params> {
    let (scale, distance) = (*HUD.lock().ok()?)?;
    if !DRIVER.capturing() {
        return None;
    }
    // A wheel held open is shown smaller than the HUD (the HUD with it, while it is open).
    let scale = if wheel_open() { scale.min(WHEEL_SCALE) } else { scale };
    let (frustum, eye, ipd) = match DRIVER.peek_label() {
        Some(label) => (eye_frustum(&label.pose, label.eye).or_else(game_frustum)?, label.eye, label.pose.ipd_or(SEPARATION.load())),
        None => (game_frustum()?, 0, 0.0),
    };
    let image = Rect::sized(size.0 as f32, size.1 as f32);
    let rect = Placement::new(frustum, eye, ipd, distance).rect(image, image.aspect(), scale).array();
    Some(eng_chr::hudover::Params { rect, cross_radius: eng_chr::hudover::CROSS_RADIUS * size.1 as f32, ..Default::default() })
}

/// The HUD's size (of the eye's vertical field) while a wheel is held open.
const WHEEL_SCALE: f32 = 0.25;
/// The game opens a wheel on a d-pad press held past a tap (a tap cycles the item instead).
const WHEEL_HOLD_MS: u64 = 210;
/// The d-pad directions that hold a wheel open: the tools' (left) and the weapons' (right).
const WHEEL_BUTTONS: u16 = monaka_core::pad::button::DPAD_LEFT | monaka_core::pad::button::DPAD_RIGHT;

/// Whether a wheel is held open: its d-pad direction held past a tap (the player's, or the
/// viewer's for a hand reached over a shoulder).
fn wheel_open() -> bool {
    static HELD_SINCE: AtomicU64 = AtomicU64::new(0);
    let held = monaka_pad::buttons(250).is_some_and(|b| b & WHEEL_BUTTONS != 0);
    let now = monaka_channel::tick().max(1);
    if !held {
        HELD_SINCE.store(0, Relaxed);
        return false;
    }
    let since = match HELD_SINCE.compare_exchange(0, now, Relaxed, Relaxed) {
        Ok(_) => now,
        Err(since) => since,
    };
    now.saturating_sub(since) >= WHEEL_HOLD_MS
}

/// The frustum eye `eye` of `pose` was rendered with: the headset's, centred, or the game's (also
/// for a pose without a field of view).
fn eye_frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
    let headset_fov = HEADSET_FOV.load(Acquire) && FRUSTA.load(Relaxed) > 0;
    if headset_fov { Frustum::from_fov(pose.fov[eye]).map(|f| f.symmetric()) } else { game_frustum() }
}

/// The game camera's own frustum (from its projection's scales).
fn game_frustum() -> Option<Frustum> {
    Frustum::centred([1.0 / GAME_SCALE[0].load(), 1.0 / GAME_SCALE[1].load()])
}

/// The player camera update is about to set `camera` from the game's backward axis, up and
/// position: the eye camera to set instead, as (back, up, position), or `None` to leave it.
pub fn eye_vectors(camera: usize, back: usize, up: usize, position: usize, baked: Option<f32>) -> Option<[crate::view::camera_update::Vec3; 3]> {
    let taken = DRIVER.take_in_turn()?;
    PLAYER_CAMERA.store(camera, Relaxed);
    let (eye, pose) = (taken.eye, taken.pose);
    *PENDING.locked() = pose;
    CURRENT_EYE.store(eye as i32, Release);
    if let Some(scale) = mem::read::<[f32; 6]>(camera + engine::CAMERA_PROJECTION) {
        GAME_SCALE[0].store(scale[0]);
        GAME_SCALE[1].store(scale[5]);
    }
    if pose.valid == 0 {
        return None;
    }
    let separation = pose.ipd_or(SEPARATION.load());
    let made = eye_camera(mem::read(back)?, mem::read(up)?, mem::read(position)?, &pose, eye, separation, EYE_TEST.load(), baked);
    let Some(m) = made else {
        EYE_REFUSALS.fetch_add(1, Relaxed);
        return None;
    };
    EYE_WRITES.fetch_add(1, Relaxed);
    let at = [m[3], m[7], m[11]];
    crate::research::history::eye_written(eye, at);
    DRIVER.camera_written(at, eye, pose);
    let column = |c: usize| crate::view::camera_update::Vec3([m[c], m[4 + c], m[8 + c], 0.0]);
    Some([column(2), column(1), column(3)])
}

/// Just before the game rebuilds `camera`'s projection from its near-plane extents: for the player
/// camera, the current eye's centred headset frustum (or, at the end, the game's own extents back).
pub fn before_frustum(camera: usize) {
    if !HEADSET_FOV.load(Acquire) || camera == 0 || camera != PLAYER_CAMERA.load(Relaxed) {
        return;
    }
    let extents = camera + engine::CAMERA_EXTENTS;
    let mut fov = FOV.locked();
    let Some(current) = mem::read::<[f32; 4]>(extents) else { return };
    // The game's own extents: these, unless they are still ours from the last rebuild.
    if fov.game.is_none() || current != fov.written {
        let [left, right, bottom, top] = current;
        if !(left < 0.0 && right > 0.0 && bottom < 0.0 && top > 0.0) {
            monaka_producer::log_first!(1, "player camera extents {current:?} are not left/right/bottom/top: headset field of view off");
            HEADSET_FOV.store(false, Release);
            return;
        }
        if fov.game.is_none() {
            log!("player camera extents {current:?} at near {:?}", mem::read::<f32>(camera + engine::CAMERA_NEAR));
        }
        fov.game = Some(current);
    }
    if fov.restore {
        if let Some(game) = fov.game
            && !fov.restored
        {
            mem::write(extents, game);
            fov.restored = true;
        }
        return;
    }
    let Ok(eye) = usize::try_from(CURRENT_EYE.load(Acquire)) else { return };
    let pose = *PENDING.locked();
    let near = mem::read::<f32>(camera + engine::CAMERA_NEAR).filter(|n| n.is_finite() && *n > 0.0);
    let (Some(frustum), Some(near)) = (Frustum::from_fov(pose.fov[eye.min(1)]).filter(|_| pose.valid != 0), near) else { return };
    let wanted = frustum.symmetric().extents(near);
    if mem::write(extents, wanted) {
        fov.written = wanted;
        FRUSTA.fetch_add(1, Relaxed);
    }
}

/// The camera-to-world matrix of eye `eye` for a game camera given as (back, up, position) and a
/// head pose in the camera's space; `test` turns eye 0 left and eye 1 right by that angle. With
/// head aim (`baked`: the head yaw it put into this camera, radians) the eyes are built on the
/// camera levelled with that yaw turned back out: the head alone turns and pitches the view.
#[allow(clippy::too_many_arguments)]
pub fn eye_camera(back: [f32; 3], up: [f32; 3], position: [f32; 3], pose: &HeadPose, eye: usize, separation: f32, test: f32, baked: Option<f32>) -> Option<Mat34> {
    let right = [up[1] * back[2] - up[2] * back[1], up[2] * back[0] - up[0] * back[2], up[0] * back[1] - up[1] * back[0]];
    let base: Mat34 = [
        right[0], up[0], back[0], position[0], //
        right[1], up[1], back[1], position[1], //
        right[2], up[2], back[2], position[2],
    ];
    if !camera::is_rigid(&base, 0.01) {
        return None;
    }
    let base = match baked {
        Some(yaw) => {
            let mut origin = camera::tracking_origin(&base, yaw)?;
            // Room-scale following: the head's movement the character has walked is in the camera.
            monaka_arms::roomscale::note_view(&origin, pose.position);
            monaka_arms::roomscale::shift_origin(&mut origin);
            origin
        }
        None => base,
    };
    let center = camera::apply_head(&base, pose.orientation, pose.position);
    let eyes = camera::parallel_eyes(&camera::rigid_inverse(&center), &center, separation)?;
    let m = eyes[eye.min(1)];
    Some(if test != 0.0 { camera::turn(&m, if eye == 0 { test } else { -test }) } else { m })
}

/// The eye cameras' counts since the last report (`seconds` ago), with the loop's own.
pub fn report_rates(seconds: f64) {
    static LAST: [AtomicU64; 1] = [AtomicU64::new(0)];
    let rate = monaka_producer::rates([EYE_WRITES.load(Relaxed)], &LAST, seconds);
    log!(
        "stereo: eye cameras {:.1}/s (refused {}), held back for the eye size {}, headset frusta {}, head {}; {}",
        rate[0],
        EYE_REFUSALS.load(Relaxed),
        SIZE_WAITS.load(Relaxed),
        FRUSTA.load(Relaxed),
        if DRIVER.head().current().is_some() { "tracked" } else { "none" },
        DRIVER.summary(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_pose() -> HeadPose {
        HeadPose { valid: 1, ipd: 0.064, ..HeadPose::default() }
    }

    #[test]
    fn eyes_straddle_the_game_camera() {
        // Looking down -z (back +z), y up, at (10, 2, 5).
        let pose = level_pose();
        let left = eye_camera([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [10.0, 2.0, 5.0], &pose, 0, 0.064, 0.0, None).unwrap();
        let right = eye_camera([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [10.0, 2.0, 5.0], &pose, 1, 0.064, 0.0, None).unwrap();
        assert!((left[3] - 9.968).abs() < 1e-4 && (right[3] - 10.032).abs() < 1e-4, "eyes along the camera's right axis");
        assert_eq!((left[7], left[11]), (2.0, 5.0));
        assert!((left[10] - 1.0).abs() < 1e-6, "same view direction");
    }

    #[test]
    fn the_head_turns_the_camera() {
        // The head turned 90 degrees left about +y: the back axis (+z) becomes +x, so the view
        // looks down -x.
        let pose = HeadPose { orientation: [0.0, (45f32).to_radians().sin(), 0.0, (45f32).to_radians().cos()], ..level_pose() };
        let eye = eye_camera([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0; 3], &pose, 0, 0.064, 0.0, None).unwrap();
        assert!((eye[2] - 1.0).abs() < 1e-5 && eye[10].abs() < 1e-5, "{eye:?}");
    }

    #[test]
    fn head_aim_levels_the_camera_and_turns_its_head_yaw_out() {
        // A game camera turned 30 degrees left (head aim baked that in) and pitched down: with the
        // head level and ahead, the eyes look level along the yaw the game had before head aim.
        let (yaw, pitch) = (30f32.to_radians(), -20f32.to_radians());
        let back = [yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let up = [yaw.sin() * pitch.sin(), pitch.cos(), yaw.cos() * pitch.sin()];
        let eye = eye_camera(back, up, [0.0; 3], &level_pose(), 0, 0.064, 0.0, Some(yaw)).unwrap();
        let view_back = [eye[2], eye[6], eye[10]];
        assert!(view_back[1].abs() < 1e-4, "level: {view_back:?}");
        assert!((view_back[2] - 1.0).abs() < 1e-4, "the baked yaw turned out: {view_back:?}");
    }

    #[test]
    fn a_skewed_camera_is_refused() {
        assert!(eye_camera([0.0, 0.0, 1.0], [0.0, 0.5, 0.5], [0.0; 3], &level_pose(), 0, 0.064, 0.0, None).is_none());
    }
}
