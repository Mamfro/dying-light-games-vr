//! Alternate-eye stereo on D3D12: after each present `n` the next eye and the head pose are chosen
//! ([`after_present`]); the game's player camera update then sets the camera to that eye
//! ([`eye_vectors`], in the `SetView(vec3)` detour), and the frame shows `latency` presents later,
//! where `monaka_stereo::alternate12` keeps it and publishes it with its partner eye.
//!
//! The camera written is the game's live player camera (there is no separate render copy to move,
//! as in DL1), so culling, shadows and the renderer's own camera copies all follow the eye. The
//! game rebuilds the camera from the character's look angles every update, so nothing stacks.
//! The field of view stays the game's; each eye's pose goes out with it, and the viewer places the
//! image accordingly.

use crate::engine;
use monaka_channel::ChannelName;
use monaka_channel::pose::{HeadSource, PoseChannel, Synthetic};
use monaka_core::alternate::Schedule;
use monaka_core::camera::{self, Frustum, Mat34};
use monaka_core::protocol::HeadPose;
use monaka_hook::{AtomicF32, mem};
use monaka_producer::log;
use monaka_stereo::alternate12::{AlternatePublisher12, Step};
use std::sync::Mutex;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain3;

pub static HEAD: HeadSource = HeadSource::new();
static PUBLISHER: Mutex<Option<AlternatePublisher12>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Presents seen since stereo started (the next one is `n`).
static PRESENTS: AtomicU64 = AtomicU64::new(0);
/// 0 once a present has been seen since stereo started (-1: leave the camera alone).
static EYE: AtomicI32 = AtomicI32::new(-1);
/// The eye the last player camera update was given (-1: none).
static CURRENT_EYE: AtomicI32 = AtomicI32::new(-1);
/// Eye choices made, one per present; the camera updates take them in turn ([`Sequencer`]).
static CHOICES: AtomicU64 = AtomicU64::new(0);
static PENDING: Mutex<HeadPose> = Mutex::new(HeadPose::EMPTY);
static PAIR_POSE: Mutex<HeadPose> = Mutex::new(HeadPose::EMPTY);
/// Eye separation (metres) when the headset reports no plausible IPD.
static SEPARATION: AtomicF32 = AtomicF32::new(0.064);
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


struct Counters {
    eye_writes: AtomicU64,
    eye_refusals: AtomicU64,
    /// Camera updates that came before the present making their choice, or after the next one.
    repeated: AtomicU64,
    skipped: AtomicU64,
    /// Times the sequence of choices was moved back into step with the presents.
    resynced: AtomicU64,
    /// Frames recognised by their camera, cameras not recognised, presents without a label once
    /// labelling works, and labels that differed from the timing-based guess.
    labelled: AtomicU64,
    label_unmatched: AtomicU64,
    unlabelled: AtomicU64,
    label_disagreed: AtomicU64,
    /// Presents held back while waiting for the headset eye size.
    size_waits: AtomicU64,
    frusta: AtomicU64,
    left: AtomicU64,
    published: AtomicU64,
    unpaired: AtomicU64,
    failed: AtomicU64,
    no_queue: AtomicU64,
}
static COUNTERS: Counters = Counters {
    eye_writes: AtomicU64::new(0),
    eye_refusals: AtomicU64::new(0),
    repeated: AtomicU64::new(0),
    skipped: AtomicU64::new(0),
    resynced: AtomicU64::new(0),
    labelled: AtomicU64::new(0),
    label_unmatched: AtomicU64::new(0),
    unlabelled: AtomicU64::new(0),
    label_disagreed: AtomicU64::new(0),
    size_waits: AtomicU64::new(0),
    frusta: AtomicU64::new(0),
    left: AtomicU64::new(0),
    published: AtomicU64::new(0),
    unpaired: AtomicU64::new(0),
    failed: AtomicU64::new(0),
    no_queue: AtomicU64::new(0),
};

pub struct Settings {
    pub channel: ChannelName,
    pub schedule: Schedule,
    pub separation: f32,
    pub synthetic: Option<Synthetic>,
    pub eye_test_degrees: f32,
    pub latency_test_degrees: f32,
    pub headset_fov: bool,
}

/// The stereo channel's name, once stereo started (the dynamic HUD's panel channels sit beside it).
static CHANNEL: Mutex<Option<ChannelName>> = Mutex::new(None);

pub fn channel() -> Option<ChannelName> {
    CHANNEL.lock().ok().and_then(|c| c.clone())
}

pub fn start(settings: Settings) {
    *CHANNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings.channel.clone());
    let poses = PoseChannel::open(&settings.channel).inspect_err(|e| log!("no pose channel ({e}): no head pose records")).ok();
    let synthetic = settings.synthetic.is_some();
    HEAD.open(poses, settings.synthetic);
    SEPARATION.store(settings.separation);
    EYE_TEST.store(settings.eye_test_degrees.to_radians());
    LATENCY_TEST.store(settings.latency_test_degrees.to_radians());
    *FOV.lock().unwrap_or_else(|e| e.into_inner()) = FovState { game: None, written: [0.0; 4], restore: false, restored: false };
    HEADSET_FOV.store(settings.headset_fov, Release);
    let test = |name: &str, degrees: f32| if degrees != 0.0 { format!(", {name} test {degrees} degrees") } else { String::new() };
    log!(
        "alternate-eye stereo on {} (latency {}, eye separation {} m, {} field of view{}{}{})",
        settings.channel.as_str(),
        settings.schedule.latency,
        settings.separation,
        if settings.headset_fov { "headset" } else { "game" },
        if synthetic { ", synthetic head" } else { "" },
        test("eye", settings.eye_test_degrees),
        test("latency", settings.latency_test_degrees),
    );
    *PUBLISHER.lock().unwrap_or_else(|e| e.into_inner()) = Some(AlternatePublisher12::new(settings.channel, settings.schedule));
    PRESENTS.store(0, Relaxed);
    CHOICES.store(0, Relaxed);
    *SEQUENCER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()) = ([None; WRITTEN_KEPT], 0);
    *LABEL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LABELLING.store(false, Relaxed);
    ENABLED.store(true, Release);
}

/// Stops writing eye cameras (the next update is the game's own again) and has the next projection
/// rebuild put the game's extents back. Hooks must still be in; returns once the extents are back
/// (or after `wait_ms`, logged).
pub fn quiet(wait_ms: u64) {
    ENABLED.store(false, Release);
    EYE.store(-1, Release);
    CURRENT_EYE.store(-1, Release);
    let pending = {
        let mut fov = FOV.lock().unwrap_or_else(|e| e.into_inner());
        fov.restore = fov.game.is_some();
        fov.restore
    };
    if pending && !monaka_producer::wait_until(wait_ms, || FOV.lock().map(|f| f.restored).unwrap_or(true)) {
        log!("the game's field of view was not put back within {wait_ms} ms (the game is paused?); it returns at its next field of view change");
    }
    HEADSET_FOV.store(false, Release);
}

/// After the hooks are out: lets go of the channel and the head.
pub fn release() {
    if let Some(mut publisher) = PUBLISHER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        log!("{} pairs published, {} failures", publisher.pairs(), publisher.failures());
        publisher.close();
    }
    HEAD.close();
}

/// Before the game's present: this frame's eye is kept or its pair published. Returns the present
/// number for [`after_present`].
pub fn before_present(swap: &IDXGISwapChain3, queue: Option<ID3D12CommandQueue>) -> Option<u64> {
    if !ENABLED.load(Acquire) {
        return None;
    }
    let n = PRESENTS.fetch_add(1, Relaxed);
    if !crate::output::video::ready() {
        // Waiting for the headset eye size: nothing is published (the channel takes the first
        // published frame's size).
        LABEL.lock().unwrap_or_else(|e| e.into_inner()).take();
        COUNTERS.size_waits.fetch_add(1, Relaxed);
        return Some(n);
    }
    let Some(queue) = queue else {
        COUNTERS.no_queue.fetch_add(1, Relaxed);
        return Some(n);
    };
    let scale = [GAME_SCALE[0].load(), GAME_SCALE[1].load()];
    let separation = SEPARATION.load();
    let headset_fov = HEADSET_FOV.load(Acquire) && COUNTERS.frusta.load(Relaxed) > 0;
    // Each eye goes out with the field of view it was rendered with: its own centred, or the game's.
    let report = |used: &mut HeadPose| {
        used.ipd = used.ipd_or(separation);
        if headset_fov {
            for fov in &mut used.fov {
                if let Some(frustum) = Frustum::from_fov(*fov) {
                    *fov = frustum.symmetric().fov();
                }
            }
        } else if scale.iter().all(|s| s.is_finite() && *s > 0.0) {
            let (x, y) = (1.0 / scale[0], 1.0 / scale[1]);
            used.fov = [Frustum { left: -x, right: x, up: y, down: -y }.fov(); 2];
        }
    };
    let label = LABEL.lock().unwrap_or_else(|e| e.into_inner()).take();
    let mut publisher = PUBLISHER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(publisher) = publisher.as_mut() {
        let scheduled = publisher.schedule().shown_eye(n);
        if let Some(shown) = label.map(|l| l.eye).or(scheduled) {
            crate::research::history::shown(shown);
        }
        let step = match label {
            // Frame generation makes and publishes the pairs itself, from labelled frames.
            Some(label) if crate::output::framegen::enabled() => crate::output::framegen::present(swap, &queue, label.eye, label.pose, &HEAD, report),
            // No eye camera drew it (a menu, the pause screen): flat to both eyes.
            None if crate::output::framegen::enabled() => {
                COUNTERS.unlabelled.fetch_add(1, Relaxed);
                crate::output::framegen::present_unlabelled(swap, &queue, &HEAD, report)
            }
            // The frame's own label: its eye and pose exactly, whatever the timing.
            Some(label) => {
                if scheduled.is_some_and(|s| s != label.eye) {
                    COUNTERS.label_disagreed.fetch_add(1, Relaxed);
                }
                publisher.show_labelled(swap, &queue, label.eye, label.pose, &HEAD, report)
            }
            // Labelled before but not this frame (no DLSS camera seen for it): its eye is unknown.
            None if LABELLING.load(Relaxed) => {
                COUNTERS.unlabelled.fetch_add(1, Relaxed);
                Step::Waiting
            }
            None => publisher.before_present(swap, &queue, n, &HEAD, report),
        };
        let counter = match step {
            Step::Waiting => None,
            Step::Left => Some(&COUNTERS.left),
            Step::Published => Some(&COUNTERS.published),
            Step::Unpaired => Some(&COUNTERS.unpaired),
            Step::Failed => Some(&COUNTERS.failed),
        };
        if let Some(counter) = counter {
            counter.fetch_add(1, Relaxed);
        }
        if step == Step::Published
            && LATENCY_TEST.load() != 0.0
            && let Some((sequence, eyes)) = publisher.last_pair()
        {
            // To match against the probe's saved pairs: which head each pair's record says.
            monaka_producer::log_first!(600, "pair {sequence}: record head yaw {:.1} degrees", monaka_core::math::yaw(eyes[0].orientation).to_degrees());
        }
    }
    Some(n)
}

/// An eye camera as written, to recognise the frame rendered from it.
#[derive(Clone, Copy)]
struct Written {
    position: [f32; 3],
    eye: usize,
    pose: HeadPose,
}

/// The frame being rendered, recognised by its camera: its eye and the pose it was drawn with.
#[derive(Clone, Copy)]
struct Label {
    eye: usize,
    pose: HeadPose,
}

const WRITTEN_KEPT: usize = 8;
/// A rendered camera this close (world units, metres) to a written one is that one.
const SAME_CAMERA: f32 = 0.005;
static WRITTEN: Mutex<([Option<Written>; WRITTEN_KEPT], usize)> = Mutex::new(([None; WRITTEN_KEPT], 0));
static LABEL: Mutex<Option<Label>> = Mutex::new(None);
/// Frames have been labelled this run, so a frame without a label is not guessed.
static LABELLING: AtomicBool = AtomicBool::new(false);

fn remember_written(written: Written) {
    let mut ring = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    let slot = ring.1 % WRITTEN_KEPT;
    ring.0[slot] = Some(written);
    ring.1 += 1;
}

/// Whether stereo runs (eye cameras are written).
pub fn running() -> bool {
    EYE.load(Acquire) >= 0
}

/// The renderer reports the camera position it renders the coming frame with (DLSS's camera
/// constants, on the render thread before that frame's present): the frame's eye and pose are those
/// of the eye camera written at that position. Returns the eye, if recognised.
pub fn label_frame(position: [f32; 3]) -> Option<usize> {
    if !ENABLED.load(Acquire) || !position.iter().all(|v| v.is_finite()) {
        return None;
    }
    let found = {
        let ring = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        let distance = |w: &Written| (0..3).map(|i| (w.position[i] - position[i]).abs()).fold(0.0, f32::max);
        ring.0.iter().flatten().filter(|w| distance(w) < SAME_CAMERA).min_by(|a, b| distance(a).total_cmp(&distance(b))).copied()
    };
    match found {
        Some(written) => {
            *LABEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Label { eye: written.eye, pose: written.pose });
            LABELLING.store(true, Relaxed);
            COUNTERS.labelled.fetch_add(1, Relaxed);
            Some(written.eye)
        }
        None => {
            COUNTERS.label_unmatched.fetch_add(1, Relaxed);
            None
        }
    }
}

/// The eye of the frame the renderer is working on now (the one the next present shows), while
/// stereo runs.
pub fn rendering_eye() -> Option<usize> {
    if !ENABLED.load(Acquire) {
        return None;
    }
    let schedule = PUBLISHER.lock().ok()?.as_ref()?.schedule();
    schedule.shown_eye(PRESENTS.load(Relaxed))
}

/// After present `n`: one more eye choice is due (the camera written after present `n` is eye
/// `Schedule::camera_eye(n)`); the camera updates take them in turn ([`Sequencer`]).
pub fn after_present(_n: u64) {
    if !ENABLED.load(Acquire) {
        return;
    }
    CHOICES.fetch_add(1, AcqRel);
    EYE.store(0, Release);
}

/// Hands eye choices to the player camera updates strictly in turn. The game thread's updates and
/// the presents match one to one, but which comes first wobbles: an update can arrive before the
/// present that makes its choice, or after the next one. Taking the choice after the last one used,
/// rather than the newest made, keeps every frame on its own eye through that wobble; only a
/// lasting offset (an update or a present that had no partner) moves it back into step.
#[derive(Default)]
struct Sequencer {
    used: u64,
    run: u32,
    run_sign: i64,
}

#[derive(Debug, PartialEq, Eq)]
enum Taken {
    InStep,
    Early,
    Late,
    Resynced,
}

impl Sequencer {
    /// Updates in a row offset the same way before the sequence follows the presents again.
    const RESYNC_AFTER: u32 = 8;

    /// The choice (1-based) for the next camera update, `made` choices having been made so far.
    fn next(&mut self, made: u64) -> (u64, Taken) {
        if self.used == 0 {
            self.used = made;
            return (made, Taken::InStep);
        }
        let choice = self.used + 1;
        let offset = (made as i64 - choice as i64).signum();
        if offset != 0 && offset == self.run_sign {
            self.run += 1;
        } else {
            self.run = u32::from(offset != 0);
            self.run_sign = offset;
        }
        if self.run >= Self::RESYNC_AFTER {
            *self = Self { used: made, run: 0, run_sign: 0 };
            return (made, Taken::Resynced);
        }
        self.used = choice;
        (choice, if offset < 0 { Taken::Early } else if offset > 0 { Taken::Late } else { Taken::InStep })
    }
}

static SEQUENCER: Mutex<Option<Sequencer>> = Mutex::new(None);

/// The player camera update is about to set `camera` from the game's backward axis, up and
/// position: the eye camera to set instead, as (back, up, position), or `None` to leave it.
pub fn eye_vectors(camera: usize, back: usize, up: usize, position: usize, baked: Option<f32>) -> Option<[crate::view::camera_update::Vec3; 3]> {
    if EYE.load(Acquire) < 0 {
        return None;
    }
    PLAYER_CAMERA.store(camera, Relaxed);
    let (choice, taken) = SEQUENCER.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().next(CHOICES.load(Acquire));
    let counter = match taken {
        Taken::InStep => None,
        Taken::Early => Some(&COUNTERS.repeated),
        Taken::Late => Some(&COUNTERS.skipped),
        Taken::Resynced => Some(&COUNTERS.resynced),
    };
    if let Some(counter) = counter {
        counter.fetch_add(1, Relaxed);
    }
    // The camera written for choice `choice` is the one "written after present n".
    let n = choice - 1;
    let eye = Schedule::camera_eye(n);
    // Both eyes of a pair are drawn from one head pose, sampled for its left-eye frame.
    let pose = {
        let mut pair = PAIR_POSE.lock().unwrap_or_else(|e| e.into_inner());
        // With frame generation each frame takes the newest pose: a generated eye stands for the
        // moment of the other eye's frame, which must be drawn from that moment's head.
        if Schedule::starts_pair(n) || crate::output::framegen::enabled() {
            *pair = HEAD.current().unwrap_or_default();
            let test = LATENCY_TEST.load();
            if test != 0.0 && pair.valid != 0 && (n / 2).is_multiple_of(2) {
                // Every other pair's head turned left: its images must show the turn its record says.
                pair.orientation = monaka_core::math::yaw_rotation(test);
            }
        }
        *pair
    };
    if let Some(publisher) = PUBLISHER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        publisher.record_pose(n, pose);
    }
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = pose;
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
        COUNTERS.eye_refusals.fetch_add(1, Relaxed);
        return None;
    };
    COUNTERS.eye_writes.fetch_add(1, Relaxed);
    crate::research::history::eye_written(eye, [m[3], m[7], m[11]]);
    remember_written(Written { position: [m[3], m[7], m[11]], eye, pose });
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
    let mut fov = FOV.lock().unwrap_or_else(|e| e.into_inner());
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
    let pose = *PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let near = mem::read::<f32>(camera + engine::CAMERA_NEAR).filter(|n| n.is_finite() && *n > 0.0);
    let (Some(frustum), Some(near)) = (Frustum::from_fov(pose.fov[eye.min(1)]).filter(|_| pose.valid != 0), near) else { return };
    let wanted = frustum.symmetric().extents(near);
    if mem::write(extents, wanted) {
        fov.written = wanted;
        COUNTERS.frusta.fetch_add(1, Relaxed);
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

pub fn report(seconds: f64) {
    static LAST: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
    let now = [&COUNTERS.published, &COUNTERS.left, &COUNTERS.eye_writes, &COUNTERS.failed].map(|c| c.load(Relaxed));
    let rate = monaka_producer::rates(now, &LAST, seconds);
    log!(
        "stereo: pairs {:.1}/s, left eyes {:.1}/s, eye cameras {:.1}/s, failures {:.1}/s (unpaired {}, refused {}, no queue {}, head {}); \
         camera updates early {}, late {}, resynced {}; frames labelled {}, unmatched {}, unlabelled {}, label differed from timing {}; held back for the eye size {}; headset frusta {}",
        rate[0],
        rate[1],
        rate[2],
        rate[3],
        COUNTERS.unpaired.load(Relaxed),
        COUNTERS.eye_refusals.load(Relaxed),
        COUNTERS.no_queue.load(Relaxed),
        if HEAD.current().is_some() { "tracked" } else { "none" },
        COUNTERS.repeated.load(Relaxed),
        COUNTERS.skipped.load(Relaxed),
        COUNTERS.resynced.load(Relaxed),
        COUNTERS.labelled.load(Relaxed),
        COUNTERS.label_unmatched.load(Relaxed),
        COUNTERS.unlabelled.load(Relaxed),
        COUNTERS.label_disagreed.load(Relaxed),
        COUNTERS.size_waits.load(Relaxed),
        COUNTERS.frusta.load(Relaxed),
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
    fn updates_take_choices_in_turn_through_the_wobble() {
        let mut s = Sequencer::default();
        assert_eq!(s.next(1), (1, Taken::InStep));
        assert_eq!(s.next(2), (2, Taken::InStep));
        // Early: a second update before the present making choice 3.
        assert_eq!(s.next(2), (3, Taken::Early));
        // Late: two presents before this update; it still takes the next choice, not the newest.
        assert_eq!(s.next(4), (4, Taken::InStep));
        assert_eq!(s.next(6), (5, Taken::Late));
        assert_eq!(s.next(6), (6, Taken::InStep));
    }

    #[test]
    fn a_lasting_offset_is_followed() {
        let mut s = Sequencer::default();
        assert_eq!(s.next(1).0, 1);
        // A present had no camera update: from here every update is one late.
        let mut made = 2;
        let mut last = (0, Taken::InStep);
        for _ in 0..Sequencer::RESYNC_AFTER {
            made += 1;
            last = s.next(made);
        }
        assert_eq!(last, (made, Taken::Resynced));
        assert_eq!(s.next(made + 1), (made + 1, Taken::InStep));
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
    fn a_skewed_camera_is_refused() {        assert!(eye_camera([0.0, 0.0, 1.0], [0.0, 0.5, 0.5], [0.0; 3], &level_pose(), 0, 0.064, 0.0, None).is_none());
    }
}
