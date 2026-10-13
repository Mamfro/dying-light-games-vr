//! Provider: the eye site (`monaka_stereo::eye_site::EyeSite`: reading the game camera and writing
//! an eye into it, installed with the present loop), on the shared recipe
//! (`monaka_core::camera::EyeView::of`). With it, what the eyes' component ([`crate::EYES`]) runs and the present loop's steps for The Beast ([`before_present`],
//! [`after_present`], [`report`]; listed by the present entry,
//! [`present12`](crate::providers::present12)).
//!
//! Alternate-eye stereo on D3D12, on Monaka's present loop (`monaka_stereo::feature`, its
//! `DRIVER`, entered from the present request: `present12`): after each present the loop
//! chooses the next eye; the game's player camera update takes the choices in turn and sets the
//! camera to that eye ([`eye_vectors`], in the `SetView(vec3)` detour), and the renderer labels
//! each frame with its eye when DLSS gets its camera ([`dlss`](crate::providers::dlss)), so the
//! frame goes out as that eye with its pose, whatever the timing.
//!
//! The camera written is the game's live player camera (there is no separate render copy to move,
//! as in DL1), so culling, shadows and the renderer's own camera copies all follow the eye. The
//! game rebuilds the camera from the character's look angles every update, so nothing stacks.
//! Each eye is rendered with the headset's field of view, centred ([`before_frustum`]), or the
//! game's; its pose goes out with the one it was rendered with ([`report`]).

use crate::providers::camera_update::Vec3;
use crate::providers::engine;
use crate::Beast;
use eng_chr::providers::look_fields::PlayerUpdate;
use monaka_core::camera::{self, Base, EyeView, Frustum, Mat34};
use monaka_core::options::Shared;
use monaka_core::protocol::HeadPose;
use monaka_core::sync::Locked;
use monaka_hook::{AtomicF32, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::log;
use monaka_stereo::driver::Driver;
use monaka_stereo::eye_site::{EyeSite, Tally};
use monaka_stereo::feature::DRIVER;
use std::sync::Mutex;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

/// The eye the last player camera update was given (-1: none), and the pose it was drawn from.
static CURRENT_EYE: AtomicI32 = AtomicI32::new(-1);
static PENDING: Mutex<HeadPose> = Mutex::new(HeadPose::EMPTY);
/// The shared settings the eyes are drawn with (eye distance, head model).
static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
/// PC check of the eye order: eye 0 turned this far left, eye 1 as far right (radians).
static EYE_TEST: AtomicF32 = AtomicF32::zero();
/// The game camera's projection scales P00 and P11 (its field of view), from the last update.
static GAME_SCALE: [AtomicF32; 2] = [AtomicF32::zero(), AtomicF32::zero()];
/// PC check of the pose records' timing: the head of every other pair turned this far (radians).
static LATENCY_TEST: AtomicF32 = AtomicF32::zero();

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

/// Player camera updates and the eye cameras written at them (the loop's rate line), refused and
/// made on a camera carrying head aim; headset frusta written.
pub static CAMERAS: Tally = Tally::new("player camera updates");
static FRUSTA: AtomicU64 = AtomicU64::new(0);

pub struct Settings {
    /// The eye distance and the head model (`head_position`) the eyes are drawn with.
    pub shared: Shared,
    pub eye_test_degrees: f32,
    pub latency_test_degrees: f32,
    pub headset_fov: bool,
    /// The HUD's size (1 its size on a monitor) and distance (metres) in front of the eyes, when
    /// it is laid over them (`eng_chr::providers::hud_over`).
    pub hud: Option<(f32, f32)>,
}

/// The run's eye settings (the loop itself is the present component's).
pub fn install(settings: Settings) {
    *SHARED.locked() = Some(settings.shared);
    EYE_TEST.store(settings.eye_test_degrees.to_radians());
    LATENCY_TEST.store(settings.latency_test_degrees.to_radians());
    *FOV.locked() = FovState { game: None, written: [0.0; 4], restore: false, restored: false };
    HEADSET_FOV.store(settings.headset_fov, Release);
    eng_chr::providers::hud_over::set(settings.hud);
    let test = |name: &str, degrees: f32| if degrees != 0.0 { format!(", {name} test {degrees} degrees") } else { String::new() };
    log!(
        "alternate-eye stereo (eye separation {} m, head position {:?}, {} field of view{}{})",
        settings.shared.eye_separation,
        settings.shared.head_position,
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
/// size (the channel takes the first published frame's size; `monaka_stereo::eye_size`), the frame
/// shown noted for the history probe. Frame warping and per-eye frame generation publish their own
/// pairs at the loop's step after this. True when the loop is to publish nothing itself.
pub fn before_present(driver: &Driver, chain: &IDXGISwapChain, n: u64) -> bool {
    monaka_producer::situation::report(monaka_producer::situation::Situation::WorldHidden, !monaka_stereo::eye_site::world_drawn_recently::<crate::Beast>(), 500);
    if monaka_stereo::eye_size::hold_labelled::<crate::Beast>(driver, chain, n) {
        return true;
    }
    let label = driver.peek_label();
    if let Some(shown) = label.map(|l| l.eye).or_else(|| driver.schedule().and_then(|s| s.shown_eye(n))) {
        crate::research::history::shown(shown);
    }
    false
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

/// The eye site as the present loop reads it, installed with it: each eye goes out with the field
/// of view it was rendered with (its own centred, or the game's) and the eye distance used
/// (`monaka_stereo::eye_site::loop_report`).
impl EyeSite for Beast {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        install_eyes(start)
    }

    fn tally() -> &'static Tally {
        &CAMERAS
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        eye_frustum(pose, eye)
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        separation(pose)
    }
}

/// The eyes' separation for `pose`, in the game's units (metres).
pub(crate) fn separation(pose: &HeadPose) -> Option<f32> {
    Some(shared()?.separation_for(pose))
}

/// The frustum eye `eye` of `pose` was rendered with: the headset's, centred, or the game's (also
/// for a pose without a field of view).
pub(crate) fn eye_frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
    let headset_fov = HEADSET_FOV.load(Acquire) && FRUSTA.load(Relaxed) > 0;
    if headset_fov { Frustum::from_fov(pose.fov[eye]).map(|f| f.symmetric()) } else { game_frustum() }
}

/// The game camera's own frustum (from its projection's scales).
pub(crate) fn game_frustum() -> Option<Frustum> {
    Frustum::centred([1.0 / GAME_SCALE[0].load(), 1.0 / GAME_SCALE[1].load()])
}

/// The game camera's full horizontal and vertical angles (radians, from its projection's scales).
fn game_fov() -> [f32; 2] {
    [GAME_SCALE[0].load(), GAME_SCALE[1].load()].map(|scale| 2.0 * (1.0 / scale).atan())
}

/// The shared settings, once installed.
fn shared() -> Option<Shared> {
    *SHARED.lock().ok()?
}

/// The player camera update (`update`) is about to set `camera`: the eye camera to set instead, as
/// (back, up, position), or `None` to leave it.
pub fn eye_vectors(camera: usize, update: &PlayerUpdate) -> Option<[Vec3; 3]> {
    let call = CAMERAS.call();
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
    // Crouching by ducking follows the head's height.
    monaka_body::duck::note_head(pose.position[1]);
    // The head yaw head aim baked into the camera (the share the update left in the targets),
    // while it steers.
    let baked = monaka_body::look::baked::<crate::Beast>(None).map(|share| share.yaw);
    if baked.is_some() {
        CAMERAS.aimed();
    }
    let made = shared().zip(game_camera(update)).and_then(|(shared, game)| eye_camera(&game, &pose, eye, &shared, game_fov(), EYE_TEST.load(), baked));
    let Some(m) = made else {
        CAMERAS.refused(call, format_args!("eye {eye}, game camera {update:?}, field of view {:?}", game_fov()));
        return None;
    };
    CAMERAS.drawn();
    let at = [m[3], m[7], m[11]];
    crate::research::history::eye_written(eye, at);
    DRIVER.camera_written(at, eye, pose);
    let column = |c: usize| Vec3([m[c], m[4 + c], m[8 + c], 0.0]);
    Some([column(2), column(1), column(3)])
}

/// The game camera the player camera update sets: its vectors made a camera as the renderer makes
/// them, the back kept and the up made square to it (`monaka_core::camera::from_back_up`). A hit
/// shakes the camera with an up that is not square to the back (by 0.02 and more): the renderer
/// draws it all the same, so it is not refused here either.
fn game_camera(update: &PlayerUpdate) -> Option<Mat34> {
    update.matrix
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

/// The camera-to-world matrix of eye `eye` for the game camera `game` (its full angles
/// `game_fov`) and a head pose in the camera's space ([`EyeView`]: the head placed by the head
/// model, the eye half the eye distance aside); `test` turns eye 0 left and eye 1 right by that
/// angle. With head aim (`baked`: the head yaw it put into this camera, radians) the eyes are built
/// on the camera levelled with that yaw turned back out, moved back by room-scale following: the
/// head alone turns and pitches the view.
pub fn eye_camera(game: &Mat34, pose: &HeadPose, eye: usize, shared: &Shared, game_fov: [f32; 2], test: f32, baked: Option<f32>) -> Option<Mat34> {
    let (game, base) = match baked {
        Some(baked_yaw) => {
            let base = Base::Level { baked_yaw };
            // Room-scale following: the head's movement the character has walked is in the camera.
            (monaka_body::roomscale::follow_view(game, base, shared.head_position.position(pose))?, base)
        }
        None => (*game, Base::Game),
    };
    let m = EyeView::of(&game, base, pose, eye.min(1), shared, true, game_fov)?.camera;
    Some(if test != 0.0 { camera::turn(&m, if eye == 0 { test } else { -test }) } else { m })
}

/// The eye cameras' counts since the last report (`seconds` ago), with the loop's own.
pub fn report_rates(seconds: f64) {
    static LAST: [AtomicU64; 1] = [AtomicU64::new(0)];
    let rate = monaka_producer::rates([CAMERAS.drawn.load(Relaxed)], &LAST, seconds);
    log!(
        "stereo: eye cameras {:.1}/s ({}), held back for the eye size {}, headset frusta {}, head {}; {}",
        rate[0],
        CAMERAS.summary(),
        monaka_stereo::eye_size::held(),
        FRUSTA.load(Relaxed),
        if DRIVER.head().current().is_some() { "tracked" } else { "none" },
        DRIVER.summary(),
    );
}

fn install_eyes(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    start.channel()?;
    let (options, shared) = (start.options, start.shared);
    install(Settings {
        shared: *shared,
        eye_test_degrees: options.angle("eye_test", 30.0).unwrap_or(0.0),
        latency_test_degrees: options.angle("latency_test", 30.0).unwrap_or(0.0),
        headset_fov: shared.headset_fov,
        hud: shared.hud.shown.then_some((shared.hud.scale, shared.hud.distance)),
    });
    Ok(())
}

#[cfg(test)]
mod lift_checks;

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::camera::{HeadPosition, from_back_up};
    use monaka_core::math::{NECK_TO_EYES, rotate};

    fn level_pose() -> HeadPose {
        HeadPose { valid: 1, ipd: 0.064, ..HeadPose::default() }
    }

    /// The default shared settings with the head model `position`.
    fn shared_with(position: HeadPosition) -> Shared {
        let shared = Shared::from_options(&monaka_core::options::Options::parse(""), &crate::manifest::MANIFESTS[0]);
        Shared { head_position: position, ..shared }
    }

    fn update(back: [f32; 3], up: [f32; 3], position: [f32; 3]) -> PlayerUpdate {
        PlayerUpdate { back, up, position, matrix: from_back_up(back, up, position) }
    }

    fn camera_of(back: [f32; 3], up: [f32; 3], position: [f32; 3]) -> Mat34 {
        game_camera(&update(back, up, position)).unwrap()
    }

    const GAME_FOV: [f32; 2] = [1.5, 1.0];

    #[test]
    fn eyes_straddle_the_game_camera() {
        // Looking down -z (back +z), y up, at (10, 2, 5).
        let (pose, shared) = (level_pose(), shared_with(HeadPosition::Lean));
        let game = camera_of([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [10.0, 2.0, 5.0]);
        let left = eye_camera(&game, &pose, 0, &shared, GAME_FOV, 0.0, None).unwrap();
        let right = eye_camera(&game, &pose, 1, &shared, GAME_FOV, 0.0, None).unwrap();
        assert!((left[3] - 9.968).abs() < 1e-4 && (right[3] - 10.032).abs() < 1e-4, "eyes along the camera's right axis");
        assert_eq!((left[7], left[11]), (2.0, 5.0));
        assert!((left[10] - 1.0).abs() < 1e-6, "same view direction");
    }

    #[test]
    fn the_head_turns_the_camera() {
        // The head turned 90 degrees left about +y: the back axis (+z) becomes +x, so the view
        // looks down -x.
        let pose = HeadPose { orientation: [0.0, (45f32).to_radians().sin(), 0.0, (45f32).to_radians().cos()], ..level_pose() };
        let game = camera_of([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0; 3]);
        let eye = eye_camera(&game, &pose, 0, &shared_with(HeadPosition::Lean), GAME_FOV, 0.0, None).unwrap();
        assert!((eye[2] - 1.0).abs() < 1e-5 && eye[10].abs() < 1e-5, "{eye:?}");
    }

    #[test]
    fn head_aim_levels_the_camera_and_turns_its_head_yaw_out() {
        // A game camera turned 30 degrees left (head aim baked that in) and pitched down: with the
        // head level and ahead, the eyes look level along the yaw the game had before head aim.
        let (yaw, pitch) = (30f32.to_radians(), -20f32.to_radians());
        let back = [yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let up = [yaw.sin() * pitch.sin(), pitch.cos(), yaw.cos() * pitch.sin()];
        let eye = eye_camera(&camera_of(back, up, [0.0; 3]), &level_pose(), 0, &shared_with(HeadPosition::Lean), GAME_FOV, 0.0, Some(yaw)).unwrap();
        let view_back = [eye[2], eye[6], eye[10]];
        assert!(view_back[1].abs() < 1e-4, "level: {view_back:?}");
        assert!((view_back[2] - 1.0).abs() < 1e-4, "the baked yaw turned out: {view_back:?}");
    }

    #[test]
    fn the_full_head_gives_the_eyes_it_gave_before() {
        // As the eyes were made before: the given axes, levelled with the baked yaw turned out under
        // head aim, the tracked head on that, the eyes either side.
        let (yaw, pitch) = (30f32.to_radians(), -20f32.to_radians());
        let back = [yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
        let up = [yaw.sin() * pitch.sin(), pitch.cos(), yaw.cos() * pitch.sin()];
        let at = [3.0, 1.5, -4.0];
        let right = monaka_core::math::cross(up, back);
        let given: Mat34 = [right[0], up[0], back[0], at[0], right[1], up[1], back[1], at[1], right[2], up[2], back[2], at[2]];
        // The head turned 0.6 rad left, then 0.2 up.
        let ((sy, cy), (sp, cp)) = (0.3f32.sin_cos(), 0.1f32.sin_cos());
        let pose = HeadPose { orientation: [cy * sp, sy * cp, -sy * sp, cy * cp], position: [0.1, 1.6, -0.2], ..level_pose() };
        let shared = shared_with(HeadPosition::Full);
        for baked in [None, Some(0.4)] {
            let origin = baked.map_or(given, |yaw| camera::tracking_origin(&given, yaw).unwrap());
            let centre = camera::apply_head(&origin, pose.orientation, pose.position);
            let before = camera::parallel_eyes(&camera::rigid_inverse(&centre), &centre, 0.064).unwrap();
            for (eye, before) in before.iter().enumerate() {
                let after = eye_camera(&camera_of(back, up, at), &pose, eye, &shared, GAME_FOV, 0.0, baked).unwrap();
                assert!(camera::distance(&after, before) < 1e-5, "{baked:?}, eye {eye}: {after:?} {before:?}");
            }
        }
    }

    #[test]
    fn the_default_head_turns_about_the_neck_without_moving_the_eyes() {
        // The head turned 40 degrees left about the neck: the tracked eyes swing left and back, the
        // lean head model keeps their centre at the game camera.
        let (s, c) = 20f32.to_radians().sin_cos();
        let orientation = [0.0, s, 0.0, c];
        let swing = rotate(orientation, NECK_TO_EYES);
        let pose = HeadPose { orientation, position: [0, 1, 2].map(|k| swing[k] - NECK_TO_EYES[k]), ..level_pose() };
        let game = camera_of([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [10.0, 2.0, 5.0]);
        let centre = |position: HeadPosition| {
            let [l, r] = [0, 1].map(|eye| eye_camera(&game, &pose, eye, &shared_with(position), GAME_FOV, 0.0, Some(0.0)).unwrap());
            [0, 1, 2].map(|k| (l[k * 4 + 3] + r[k * 4 + 3]) * 0.5)
        };
        let lean = centre(HeadPosition::Lean);
        assert!((0..3).all(|k| (lean[k] - game[k * 4 + 3]).abs() < 1e-5), "{lean:?}");
        assert!(monaka_core::math::distance(centre(HeadPosition::Full), lean) > 0.02, "full: the eyes swing with the neck");
    }

    #[test]
    fn a_shaken_camera_is_drawn_square_and_an_impossible_one_refused() {
        // A hit's shake: the up 0.023 off square to the back (as logged). Drawn, as the renderer
        // draws it: the back kept, the up made square.
        let back = [-0.864_596_55, -0.051_314_674, -0.499_839_75];
        let shaken = camera_of(back, [-0.066_191_9, 0.997_211_6, -0.034_462_232], [0.0; 3]);
        assert!(camera::is_rigid(&shaken, 1e-4));
        assert!((0..3).all(|k| (shaken[k * 4 + 2] - back[k]).abs() < 1e-4), "the back kept");
        // No up to square (along the back) or no field of view a game has: refused.
        assert!(game_camera(&update([0.0, 0.0, 1.0], [0.0, 0.0, 2.0], [0.0; 3])).is_none());
        let game = camera_of([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0; 3]);
        assert!(eye_camera(&game, &level_pose(), 0, &shared_with(HeadPosition::Lean), [3.1, 1.0], 0.0, None).is_none(), "wider than a game's");
    }
}
