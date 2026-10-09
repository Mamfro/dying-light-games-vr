//! HUD world markers (quest objectives, waypoints): the HUD places each one through the game's
//! world-to-screen projector (`engine::HUD_PROJECT`, called from `engine::HUD_PROJECT_CALLERS`;
//! found by farmerarmor/DyingLight2VR, MIT). Hooking it gives every marker's world point each
//! frame, so depth stereo can lift markers out of the compacted HUD and draw them over their
//! targets at their own distance.
//!
//! Its probe (`probe_markers`) is in `research::markers`.

use crate::engine;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

pub type ProjectFn = unsafe extern "system" fn(out: *mut f32, world: *const f32, camera: usize, clamp: bool, clipped: *mut bool, option: bool) -> *mut f32;
pub static PROJECT: Original<ProjectFn> = Original::new();

/// The markers are taken out of the flat HUD and drawn at their targets' depth (`world_markers`).
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Markers kept per tick.
const MOST: usize = 32;

pub fn enable(on: bool) {
    ENABLED.store(on, Release);
}

/// A HUD marker of the latest tick: where the game drew it (the HUD layout's pixels) and how far
/// its target is from the frame's camera (metres).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    pub screen: [f32; 2],
    pub distance: f32,
}

/// The markers the HUD placed in its latest tick, at present `present` (none when the frame's
/// camera is unknown yet, or the calls are older than two presents).
pub fn latest(present: u64) -> Vec<Marker> {
    let Some(camera) = CAMERA.lock().ok().and_then(|c| *c) else { return Vec::new() };
    let recent = RECENT.lock().map(|r| r.clone()).unwrap_or_default();
    let newest = recent.iter().map(|c| c.present).max().unwrap_or(0);
    if present.saturating_sub(newest) > 2 {
        return Vec::new();
    }
    let mut markers: Vec<Marker> = Vec::new();
    // Off-screen markers come back clipped, or at (-1000, -1000) when behind the camera.
    for call in recent.iter().filter(|c| c.present == newest && !c.clipped && c.out[0] >= 0.0 && c.out[1] >= 0.0) {
        let d: f32 = (0..3).map(|i| (call.world[i] - camera.position[i]).powi(2)).sum::<f32>().sqrt();
        if !d.is_finite() || d < 0.05 {
            continue;
        }
        let marker = Marker { screen: [call.out[0], call.out[1]], distance: d };
        // One per place: a marker and its edge-clamp call land on the same point.
        if markers.iter().any(|m| (m.screen[0] - marker.screen[0]).abs() < 1.0 && (m.screen[1] - marker.screen[1]).abs() < 1.0) {
            continue;
        }
        if markers.len() < MOST {
            markers.push(marker);
        }
    }
    crate::research::markers::latest(present, camera.position, &markers);
    markers
}

static GAMEDLL: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static HUD_CALLS: AtomicU64 = AtomicU64::new(0);

/// The frame's camera from the DLSS constants: its position.
#[derive(Clone, Copy)]
struct FrameCamera {
    position: [f32; 3],
}

static CAMERA: Mutex<Option<FrameCamera>> = Mutex::new(None);

/// One HUD projector call.
#[derive(Clone, Copy)]
pub(crate) struct Call {
    pub present: u64,
    pub caller: usize,
    pub world: [f32; 3],
    pub out: [f32; 4],
    pub clipped: bool,
    pub clamp: bool,
}

/// The calls of the latest frames.
static RECENT: Mutex<Vec<Call>> = Mutex::new(Vec::new());

/// The calls of the latest frames (for `research::markers`' dumps).
pub(crate) fn recent() -> Vec<Call> {
    RECENT.lock().map(|r| r.clone()).unwrap_or_default()
}

/// Hooks the projector after checking its prologue and that every HUD caller calls it.
///
/// # Safety
/// `gamedll` is the inspected build of the game DLL.
pub unsafe fn install(hooks: &mut monaka_hook::Hooks, gamedll: &monaka_hook::module::Module) -> Result<(), monaka_producer::Rejection> {
    let target = gamedll.at(engine::HUD_PROJECT.0);
    for &rva in &engine::HUD_PROJECT_CALLERS {
        let call: Option<[u8; 5]> = mem::read(gamedll.at(rva - 5));
        let lands = call.is_some_and(|c| c[0] == 0xe8 && (rva as i64 + i32::from_le_bytes([c[1], c[2], c[3], c[4]]) as i64) == engine::HUD_PROJECT.0 as i64);
        if !lands {
            return Err(monaka_producer::Rejection::revision(format!("HUD marker caller {rva:#x} does not call the projector")));
        }
    }
    GAMEDLL.store(gamedll.base(), Release);
    // SAFETY: the prologue is checked by `inline`; the detour has the projector's type.
    unsafe { hooks.inline(&PROJECT, "HUD marker projector", target, engine::HUD_PROJECT.1, project as ProjectFn) }?;
    Ok(())
}

/// The frame's camera, from the DLSS constants the game set (depth stereo: the centre camera).
pub fn set_camera(constants: &monaka_streamline::Constants) {
    use monaka_streamline::constants as at;
    crate::research::markers::constants(constants);
    if constants.view_projection().is_none() {
        return;
    }
    let camera = FrameCamera { position: constants.vec3(at::POSITION) };
    if let Ok(mut slot) = CAMERA.lock() {
        *slot = Some(camera);
    }
}

unsafe extern "system" fn project(out: *mut f32, world: *const f32, camera: usize, clamp: bool, clipped: *mut bool, option: bool) -> *mut f32 {
    let _flight = InFlight::enter();
    // SAFETY: the game's own call, forwarded unchanged.
    let result = unsafe { PROJECT.get()(out, world, camera, clamp, clipped, option) };
    CALLS.fetch_add(1, Relaxed);
    if !crate::research::options().markers && !ENABLED.load(Relaxed) {
        return result;
    }
    let caller = monaka_hook::probe::caller().wrapping_sub(GAMEDLL.load(Relaxed));
    if !engine::HUD_PROJECT_CALLERS.contains(&caller) {
        return result;
    }
    HUD_CALLS.fetch_add(1, Relaxed);
    let (Some(world), Some(out_values)) = (mem::read::<[f32; 3]>(world as usize), mem::read::<[f32; 4]>(result as usize)) else { return result };
    let call = Call {
        present: crate::PRESENTS.load(Relaxed),
        caller,
        world,
        out: out_values,
        clipped: !clipped.is_null() && mem::read::<u8>(clipped as usize).is_some_and(|c| c != 0),
        clamp,
    };
    if let Ok(mut recent) = RECENT.lock() {
        recent.retain(|c| call.present.saturating_sub(c.present) <= 2);
        if recent.len() < 512 {
            recent.push(call);
        }
    }
    crate::research::markers::call(&call, camera, option);
    result
}

pub fn report() {
    if GAMEDLL.load(Relaxed) != 0 {
        log!("HUD marker projector: calls={} from the HUD={}", CALLS.load(Relaxed), HUD_CALLS.load(Relaxed));
    }
}
