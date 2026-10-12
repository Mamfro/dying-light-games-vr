//! The grappling hook by hand (with the hand rig): the hook goes where the left controller points
//! (its reticle), not where the head looks.
//!
//! The hook's rope (`RopeLocomotionController`) finds what it can catch on ([`search`], from the
//! player's update and as the rope aims) along the line from its start toward the screen's centre
//! in the world (the engine's `IBaseCamera::GetOnScreenPointF`, which the fixed UI hooks and hands
//! here, [`on_screen_point`]): while the search runs, that point is the one the viewer's left
//! reticle sits on ([`note_reticle`], from the arms callback).

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

/// `RopeLocomotionController`'s search for what the hook catches on: `(controller, seconds)`.
const SEARCH: usize = 0xd76000;
/// A reticle older than this is not where the controller is.
const FRESH: Duration = Duration::from_millis(200);

type SearchFn = unsafe extern "C" fn(*mut c_void, f32);

static SEARCH_ORIGINAL: Original<SearchFn> = Original::new();
/// Where the viewer's left reticle sits in the world, and when it was noted.
static RETICLE: Mutex<Option<([f32; 3], Instant)>> = Mutex::new(None);
static AIMED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// While the rope's search runs on this thread.
    static SEARCHING: Cell<bool> = const { Cell::new(false) };
}

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the detour has the function's signature (read from its code); the game DLL's build is
    // checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&SEARCH_ORIGINAL, "rope search", gamedll.at(SEARCH), search as SearchFn)? };
    log!("grappling hook by hand: the hook goes where the left controller points");
    Ok(())
}

/// Each arms callback: where the viewer's left reticle is in the world.
pub fn note_reticle(point: Option<[f32; 3]>) {
    if let (Some(point), Ok(mut reticle)) = (point, RETICLE.lock()) {
        *reticle = Some((point, Instant::now()));
    }
}

unsafe extern "C" fn search(controller: *mut c_void, seconds: f32) {
    let _flight = InFlight::enter();
    SEARCHING.with(|s| s.set(true));
    // SAFETY: forwards the game's own call.
    unsafe { SEARCH_ORIGINAL.get()(controller, seconds) };
    SEARCHING.with(|s| s.set(false));
}

/// The camera's world point under a screen point, as the engine made it at `result`: while the
/// rope searches, the left reticle's.
pub fn on_screen_point(result: usize) {
    if !SEARCH_ORIGINAL.is_set() || !SEARCHING.with(Cell::get) {
        return;
    }
    let Some(point) = RETICLE.lock().ok().and_then(|r| *r).filter(|(_, at)| at.elapsed() < FRESH).map(|(p, _)| p) else { return };
    mem::write::<[f32; 3]>(result, point);
    let n = AIMED.fetch_add(1, Relaxed) + 1;
    if n == 1 || n.is_power_of_two() {
        log!("grappling hook by hand: the hook searched toward the left reticle at {point:.2?} ({n})");
    }
}

pub fn report() {
    let n = AIMED.load(Relaxed);
    if n > 0 {
        log!("grappling hook by hand: the hook searched toward the left reticle {n} times");
    }
}
