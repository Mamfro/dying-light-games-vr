//! Per-eye DLSS (`dlss_per_eye=1`): in alternate-eye stereo the game's previous frame is the other
//! eye's, so DLSS would carry one eye into the other. Each frame's eye is known from its label
//! (`stereo::label_frame`, at the camera constants), and then:
//!
//! - constants, tags and evaluate go to that eye's own viewport (a copy of the game's handle with
//!   the eye in its top bits), so DLSS keeps one history per eye; the game's DLSS options are
//!   passed on to both eye viewports when it sets them (`slDLSSSetOptions`, through the pointer
//!   rd3d12 keeps), and nothing changes until they have been;
//! - `clipToPrevClip`/`prevClipToClip` describe that eye's own previous frame (two frames back),
//!   computed from the cameras as the game's are (checked to match within float rounding);
//! - the motion vectors are rebased onto that previous frame on the GPU (`monaka_framegen::motion`), into a
//!   texture of our own given to DLSS in the game's place.
//!
//! All of it runs on the render thread inside the game's own Streamline calls.

use monaka_framegen::motion::{self, MotionFix};
use monaka_channel::d3d12;
use monaka_core::Aligned16;
use monaka_core::depth::{invert, multiply};
use monaka_hook::mem;
use monaka_streamline::{Constants, TAG_BYTES, buffer, constants as at};
use monaka_producer::log;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D12::{ID3D12GraphicsCommandList, ID3D12Resource};
use windows::core::Interface;

pub static ENABLED: AtomicBool = AtomicBool::new(false);
/// The motion-vector rebase (`dlss_fix_motion=0` leaves the game's vectors, to test the viewports
/// and constants alone).
pub static FIX_MOTION: AtomicBool = AtomicBool::new(true);

/// The jitter sequence per eye: the game takes frame `n`'s sub-pixel jitter from a sequence whose
/// steps alternate sides (x positive on even steps, negative on odd), so with the eyes alternating
/// each eye's own DLSS history would only ever sample half of every pixel. Frame `n` is given step
/// `n / 2` instead: both frames of a pair share a step, and each eye walks the whole sequence.
pub static FRAME_JITTER: monaka_hook::Original<crate::engine::FrameJitterFn> = monaka_hook::Original::new();
pub static JITTER_PER_EYE: AtomicBool = AtomicBool::new(false);

pub unsafe extern "system" fn frame_jitter(out: *mut [f32; 2], frame: u64) -> *mut [f32; 2] {
    let _flight = monaka_hook::InFlight::enter();
    let frame = if JITTER_PER_EYE.load(Relaxed) && ENABLED.load(Relaxed) { frame / 2 } else { frame };
    // SAFETY: the original, with the game's output pointer and the step chosen.
    unsafe { FRAME_JITTER.get()(out, frame) }
}
const VIEWPORT_BYTES: usize = 40;
const RESOURCE_BYTES: usize = 128;
const OPTIONS_BYTES: usize = 512;
/// How far object motion is carried: the eye's previous frame is two frames back.
const OBJECT_SCALE: f32 = 2.0;

type Bytes<const N: usize> = Aligned16<[u8; N]>;

struct State {
    /// The game's viewport handle, and each eye's copy.
    game_viewport: Option<u32>,
    eye_viewports: [Bytes<VIEWPORT_BYTES>; 2],
    options_applied: bool,
    /// This frame's eye (from its label), and what the fix needs for it.
    eye: Option<usize>,
    previous: [Option<[f32; 16]>; 2],
    fix: Option<motion::Constants>,
    depth: Option<usize>,
    motion: Option<(usize, u32)>,
    /// Kept alive for the game's calls: the rewritten constants, tags and the motion vectors'
    /// resource description.
    constants: Option<Constants>,
    tags: Vec<u8>,
    motion_resource: Bytes<RESOURCE_BYTES>,
    fixer: Option<MotionFix>,
    broken: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    game_viewport: None,
    eye_viewports: [Aligned16([0; VIEWPORT_BYTES]); 2],
    options_applied: false,
    eye: None,
    previous: [None; 2],
    fix: None,
    depth: None,
    motion: None,
    constants: None,
    tags: Vec::new(),
    motion_resource: Aligned16([0; RESOURCE_BYTES]),
    fixer: None,
    broken: false,
});

struct Counters {
    options: AtomicU64,
    frames: AtomicU64,
    fixed: AtomicU64,
    passed: AtomicU64,
}
static COUNTERS: Counters = Counters { options: AtomicU64::new(0), frames: AtomicU64::new(0), fixed: AtomicU64::new(0), passed: AtomicU64::new(0) };

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Each eye's viewport: the game's handle with the eye in the top bits of its value.
fn make_eye_viewports(s: &mut State, viewport: *const u8) -> bool {
    let mut handle = [0u8; VIEWPORT_BYTES];
    if !mem::read_bytes(viewport as usize, &mut handle) {
        return false;
    }
    let id = u32::from_le_bytes(handle[32..36].try_into().expect("4 bytes"));
    if s.game_viewport == Some(id) {
        return true;
    }
    s.game_viewport = Some(id);
    for eye in 0..2 {
        let mut copy = handle;
        copy[32..36].copy_from_slice(&monaka_streamline::eye_viewport(id, eye as u32 + 1).to_le_bytes());
        s.eye_viewports[eye] = Aligned16(copy);
    }
    true
}

/// The game sets its DLSS options for its viewport: the same options for both eyes' viewports.
/// `set` is Streamline's own `slDLSSSetOptions`.
pub fn on_options(viewport: *const u8, options: *const u8, set: impl Fn(*const u8, *const u8) -> i32) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    let mut s = state();
    if !make_eye_viewports(&mut s, viewport) {
        return;
    }
    let mut copy = Aligned16([0u8; OPTIONS_BYTES]);
    if !mem::read_bytes(options as usize, &mut copy.0) {
        return;
    }
    let results = [set(s.eye_viewports[0].0.as_ptr(), copy.0.as_ptr()), set(s.eye_viewports[1].0.as_ptr(), copy.0.as_ptr())];
    let first = !s.options_applied;
    s.options_applied = results == [0, 0];
    COUNTERS.options.fetch_add(1, Relaxed);
    if first || !s.options_applied {
        log!("DLSS options passed on to both eye viewports (results {results:?}); per-eye DLSS {}", if s.options_applied { "on" } else { "off" });
    }
}

/// `slSetConstants` for a frame labelled `eye`: the constants and viewport to pass on instead, or
/// `None` to pass the game's.
pub fn constants(constants: *const u8, viewport: *const u8, eye: Option<usize>) -> Option<(*const u8, *const u8)> {
    if !ENABLED.load(Relaxed) {
        return None;
    }
    let mut s = state();
    s.eye = None;
    s.fix = None;
    s.depth = None;
    s.motion = None;
    let eye = eye?;
    if s.broken || !s.options_applied || !make_eye_viewports(&mut s, viewport) {
        COUNTERS.passed.fetch_add(1, Relaxed);
        return None;
    }
    let mut copy = Constants::read(constants)?;
    let now = copy.view_projection()?;
    let game_to_prev = copy.matrix(at::CLIP_TO_PREV_CLIP);
    let mvec_scale = copy.vec2(at::MVEC_SCALE);
    let same_to_prev = match (s.previous[eye], invert(&now)) {
        (Some(previous), Some(inverse)) => {
            let to_prev = multiply(&inverse, &previous);
            if let Some(from_prev) = invert(&previous).map(|p| multiply(&p, &now)) {
                copy.set_matrix(at::CLIP_TO_PREV_CLIP, &to_prev);
                copy.set_matrix(at::PREV_CLIP_TO_CLIP, &from_prev);
            }
            to_prev
        }
        _ => {
            // This eye's first frame: nothing to carry over.
            copy.set_reset(true);
            game_to_prev
        }
    };
    s.previous[eye] = Some(now);
    s.eye = Some(eye);
    s.fix = Some(motion::Constants { game_to_prev, same_to_prev, size: [0; 2], mvec_scale, object_scale: OBJECT_SCALE, pad: [0.0; 3] });
    s.constants = Some(copy);
    COUNTERS.frames.fetch_add(1, Relaxed);
    Some((s.constants.as_ref()?.as_ptr(), s.eye_viewports[eye].0.as_ptr()))
}

/// `slSetTagForFrame` in a per-eye frame: the tags and viewport to pass on instead (the motion
/// vectors swapped for our output), or `None` to pass the game's.
pub fn tags(viewport: *const u8, tags: *const u8, count: u32) -> Option<(*const u8, *const u8)> {
    if !ENABLED.load(Relaxed) || tags.is_null() {
        return None;
    }
    let mut s = state();
    let eye = s.eye?;
    let id = mem::read::<u32>(viewport as usize + 32)?;
    if s.game_viewport != Some(id) {
        return None;
    }
    let mut copy = vec![0u8; count as usize * TAG_BYTES];
    if !mem::read_bytes(tags as usize, &mut copy) {
        return None;
    }
    for i in 0..count as usize {
        let at = i * TAG_BYTES;
        let resource = usize::from_le_bytes(copy[at + 32..at + 40].try_into().expect("8 bytes"));
        let kind = u32::from_le_bytes(copy[at + 40..at + 44].try_into().expect("4 bytes"));
        if resource == 0 {
            continue;
        }
        let native = mem::read::<usize>(resource + 40).unwrap_or(0);
        let state = mem::read::<u32>(resource + 64).unwrap_or(0);
        match kind {
            buffer::DEPTH => s.depth = Some(native),
            buffer::MOTION_VECTORS if FIX_MOTION.load(Relaxed) => {
                // Our output in the game's place: its description with our texture.
                let raw = native as *mut c_void;
                // SAFETY: the tagged native texture, live while the game records this frame.
                let Some(texture) = (native != 0).then(|| unsafe { ID3D12Resource::from_raw_borrowed(&raw) }).flatten() else { continue };
                // SAFETY: reads a descriptor.
                let desc = unsafe { texture.GetDesc() };
                if s.fixer.is_none() && !s.broken {
                    match d3d12::device_of(texture).map_err(|e| e.to_string()).and_then(|device| MotionFix::new(&device)) {
                        Ok(fixer) => {
                            log!("per-eye motion vectors ready");
                            s.fixer = Some(fixer);
                        }
                        Err(why) => {
                            log!("per-eye DLSS off: {why}");
                            s.broken = true;
                        }
                    }
                }
                let Some(output) = s.fixer.as_mut().and_then(|f| f.output(desc.Width as u32, desc.Height).ok()) else { continue };
                let mut description = Aligned16([0u8; RESOURCE_BYTES]);
                if !mem::read_bytes(resource, &mut description.0) {
                    continue;
                }
                description.0[40..48].copy_from_slice(&(output.as_raw() as usize).to_le_bytes());
                description.0[64..68].copy_from_slice(&(motion::READABLE.0 as u32).to_le_bytes());
                s.motion_resource = description;
                let ours = s.motion_resource.0.as_ptr() as usize;
                copy[at + 32..at + 40].copy_from_slice(&ours.to_le_bytes());
                s.motion = Some((native, state));
            }
            _ => {}
        }
    }
    s.tags = copy;
    Some((s.tags.as_ptr(), s.eye_viewports[eye].0.as_ptr()))
}

/// `slEvaluateFeature` for DLSS in a per-eye frame: records the motion-vector rebase on the native
/// command list and returns the inputs to pass on (the eye's viewport in the game's place).
pub fn evaluate(inputs: *const *const u8, count: u32, native_list: Option<ID3D12GraphicsCommandList>) -> Option<Vec<*const u8>> {
    if !ENABLED.load(Relaxed) {
        return None;
    }
    let mut s = state();
    let eye = s.eye?;
    let game = s.game_viewport?;
    let replace = |s: &State| -> Vec<*const u8> {
        (0..count as usize)
            .map(|i| {
                let input = mem::read::<usize>(inputs as usize + 8 * i).unwrap_or(0) as *const u8;
                let is_viewport = input as usize != 0 && mem::read::<u32>(input as usize + 32) == Some(game);
                if is_viewport { s.eye_viewports[eye].0.as_ptr() } else { input }
            })
            .collect()
    };
    if !FIX_MOTION.load(Relaxed) {
        return Some(replace(&s));
    }
    let (Some(fix), Some(depth), Some((motion, _))) = (s.fix, s.depth, s.motion) else { return None };
    let list = native_list?;
    let (raw_depth, raw_motion) = (depth as *mut c_void, motion as *mut c_void);
    // SAFETY: the game's tagged textures, live while it records this frame.
    let (Some(depth), Some(motion)) = (unsafe { ID3D12Resource::from_raw_borrowed(&raw_depth) }, unsafe { ID3D12Resource::from_raw_borrowed(&raw_motion) }) else {
        return None;
    };
    let recorded = s.fixer.as_mut()?.record(&list, depth, motion, &fix);
    if let Err(why) = recorded {
        log!("per-eye DLSS off: {why}");
        s.broken = true;
        return None;
    }
    COUNTERS.fixed.fetch_add(1, Relaxed);
    Some(replace(&s))
}

pub fn report() -> String {
    format!(
        "per-eye DLSS: options passed on {}, frames {}, motion vectors fixed {}, passed through {}",
        COUNTERS.options.load(Relaxed),
        COUNTERS.frames.load(Relaxed),
        COUNTERS.fixed.load(Relaxed),
        COUNTERS.passed.load(Relaxed)
    )
}

/// The eye viewports Streamline holds DLSS resources for (to free at the end).
pub fn eye_viewports() -> Option<[[u8; VIEWPORT_BYTES]; 2]> {
    let s = state();
    s.game_viewport.map(|_| [s.eye_viewports[0].0, s.eye_viewports[1].0])
}
