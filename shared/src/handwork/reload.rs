//! Reloading by hand (`manual_reload`, with the hand rig): the gun's reload runs only while the
//! left hand is at the gun (within [`REACH`] of the right hand), so the player works the magazine
//! or feeds the rounds instead of watching the hands do it.
//!
//! The game reloads in steps (magazine out and in, a round at a time for shotguns, chambering),
//! built from the reload animation's events, run on time by one function that each frame adds dt
//! (times the reload speed) to the current step's time and acts at each event: the rounds go in at
//! the game's own event. Here that function gets dt only while the left hand is at the gun; away
//! from it the reload waits where it is. Without both hands tracked it runs as the game's.

use crate::hands;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::{Rejection, log};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// One build's reload step runner (game DLL RVAs).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// Runs the reload's steps `(controller, dt) -> done`.
    pub steps: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The left hand this near the right (metres) works the reload.
const REACH: f32 = 0.22;
/// The hands' places must be this recent (seconds).
const FRESH: f32 = 0.1;

type StepsFn = unsafe extern "C" fn(usize, f32) -> bool;

static STEPS_ORIGINAL: Original<StepsFn> = Original::new();
static PLAYER_VTABLE: OnceLock<usize> = OnceLock::new();
static AT_GUN: AtomicBool = AtomicBool::new(false);
static WORKED: AtomicU64 = AtomicU64::new(0);
static WAITED: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s reload step runner in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = PLAYER_VTABLE.set(gamedll.at(build.player_vtable));
    // SAFETY: the detour has the runner's signature (as the gun's update calls it); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&STEPS_ORIGINAL, "reload steps", gamedll.at(build.steps), steps as StepsFn)? };
    Ok(())
}

/// Whether the left hand is at the gun hand now; none without both hands tracked.
fn left_hand_at_gun() -> Option<bool> {
    let left = hands::position(&hands::palm(LEFT_HAND, FRESH)?);
    let right = hands::position(&hands::palm(RIGHT_HAND, FRESH)?);
    Some(hands::length([0, 1, 2].map(|k| left[k] - right[k])) <= REACH)
}

unsafe extern "C" fn steps(controller: usize, dt: f32) -> bool {
    let _flight = InFlight::enter();
    let players = PLAYER_VTABLE.get().is_some_and(|&v| super::throwing::players(controller, v));
    let dt = match players.then(left_hand_at_gun).flatten() {
        Some(true) => {
            if !AT_GUN.swap(true, Relaxed) && WORKED.fetch_add(1, Relaxed) < 40 {
                log!("reload by hand: the left hand at the gun, reloading");
            }
            dt
        }
        Some(false) => {
            if AT_GUN.swap(false, Relaxed) && WAITED.fetch_add(1, Relaxed) < 40 {
                log!("reload by hand: the left hand left the gun, the reload waits");
            }
            0.0
        }
        None => dt,
    };
    // SAFETY: forwards the game's own call (dt possibly held at 0).
    unsafe { STEPS_ORIGINAL.get()(controller, dt) }
}

pub fn report() {
    if PLAYER_VTABLE.get().is_some() {
        log!("reload by hand: the left hand came to the gun {} times, left it {} times", WORKED.load(Relaxed), WAITED.load(Relaxed));
    }
}
