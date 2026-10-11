//! The bow by hand (with the hand rig): the bow in the left hand, the string in the right (the rig
//! holds them apart, [`crate::fpp::is_bow`]). Holding fire draws as before; how far the string hand
//! is pulled back along the bow hand's aim sets the draw's power, and the arrow leaves from the bow
//! hand along where the left controller points (the line its reticle sits on), instead of from the
//! eye along the look. While a bow is held the aim stays the head's ([`super::head_aims`]).
//!
//! The bow controller (`WeaponBowController`) draws in one state: each update it works the power
//! out from the time drawn (now less the draw's start) over the full draw time, through a curve.
//! Here, before each update in that state, the draw's start is set to now less the pull's share of
//! the full draw time, so the game's own curve, stamina and arrow speed follow the pull. Each arrow
//! is made by one call, which takes its start from the player's eye point and its aim from the
//! look: during that call both come from the hands ([`super::aim::with`]).

use super::aim;
use crate::hands;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One build's bow controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The controller's update `(controller, dt)`.
    pub update: usize,
    /// One arrow `(controller, target or 0)`.
    pub arrow: usize,
    /// The full draw time `(controller) -> seconds`.
    pub full_draw: usize,
    /// The controller's clock: vtable slot `() -> seconds`.
    pub clock_slot: usize,
    /// The controller's state (i32); [`DRAWING`] is the draw.
    pub state: usize,
    /// When the draw started (f32, the clock's seconds).
    pub draw_start: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The draw state.
const DRAWING: i32 = 3;
/// The string hand this far behind the bow hand along its aim (metres) is no draw...
const REST: f32 = 0.1;
/// ...and this far a full one.
const FULL_DRAW: f32 = 0.55;
/// The hands' places must be this recent (seconds).
const FRESH: f32 = 0.1;
/// The arrow starts this far ahead of the bow hand (metres), past the bow.
const AHEAD_OF_BOW: f32 = 0.1;

type UpdateFn = unsafe extern "C" fn(usize, f32);
type ArrowFn = unsafe extern "C" fn(usize, usize);
type SecondsFn = unsafe extern "C" fn(usize) -> f32;

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static ARROW_ORIGINAL: Original<ArrowFn> = Original::new();
/// The build, and the full draw time and player vtable relocated.
static BUILD: OnceLock<(Build, usize, usize)> = OnceLock::new();
static ARROWS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s bow update and arrow in the game DLL (and the aim part, `aim_build`).
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build, aim_build: aim::Build) -> Result<(), Rejection> {
    aim::install(hooks, gamedll, aim_build)?;
    let _ = BUILD.set((build, gamedll.at(build.full_draw), gamedll.at(build.player_vtable)));
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&UPDATE_ORIGINAL, "bow update", gamedll.at(build.update), update as UpdateFn)?;
        hooks.inline_decoded(&ARROW_ORIGINAL, "bow arrow", gamedll.at(build.arrow), arrow as ArrowFn)?;
    }
    Ok(())
}

/// The bow hand's place and aim (unit, where the left controller points) now (world).
fn bow_now() -> Option<([f32; 3], [f32; 3])> {
    let palm = hands::palm(LEFT_HAND, FRESH)?;
    let aim = hands::forward(&palm);
    let length = hands::length(aim);
    (length > 1e-3).then(|| (hands::position(&palm), aim.map(|c| c / length)))
}

/// How far the string hand is behind the bow hand along its aim (metres).
fn drawn(bow: [f32; 3], aim: [f32; 3]) -> Option<f32> {
    let string = hands::position(&hands::palm(RIGHT_HAND, FRESH)?);
    Some(hands::dot([0, 1, 2].map(|k| bow[k] - string[k]), aim))
}

/// The draw's share (0..1) for the string `back` metres behind the bow.
fn pull(back: f32) -> f32 {
    ((back - REST) / (FULL_DRAW - REST)).clamp(0.0, 1.0)
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    if let Some(&(build, full_draw, player_vtable)) = BUILD.get()
        && mem::read::<i32>(controller + build.state) == Some(DRAWING)
        && super::throwing::players(controller, player_vtable)
        && let Some(back) = bow_now().and_then(|(bow, aim)| drawn(bow, aim))
    {
        let share = pull(back);
        let clock = mem::read::<usize>(controller).and_then(|vtable| mem::read::<usize>(vtable + build.clock_slot));
        if let Some(clock) = clock {
            // SAFETY: the controller's own clock and the full draw time, no arguments beyond it,
            // as the draw itself calls them on this thread.
            let (now, full) = unsafe { (std::mem::transmute::<usize, SecondsFn>(clock)(controller), std::mem::transmute::<usize, SecondsFn>(full_draw)(controller)) };
            if now.is_finite() && full.is_finite() && full > 0.0 {
                mem::write(controller + build.draw_start, now - share * full);
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
}

unsafe extern "C" fn arrow(controller: usize, target: usize) {
    let _flight = InFlight::enter();
    let aimed = BUILD
        .get()
        .filter(|(_, _, player_vtable)| super::throwing::players(controller, *player_vtable))
        .and_then(|_| bow_now())
        .map(|(bow, aim)| ([0, 1, 2].map(|k| bow[k] + aim[k] * AHEAD_OF_BOW), aim));
    match aimed {
        Some((eye, look)) => {
            if ARROWS.fetch_add(1, Relaxed) < 40 {
                log!("bow by hand: an arrow along {look:.2?}");
            }
            // SAFETY: forwards the game's own call, with the hands' aim on this thread.
            aim::with(eye, look, || unsafe { ARROW_ORIGINAL.get()(controller, target) })
        }
        // SAFETY: forwards the game's own call.
        None => unsafe { ARROW_ORIGINAL.get()(controller, target) },
    }
}

/// Whether the bow by hand is on.
pub fn installed() -> bool {
    BUILD.get().is_some() && UPDATE_ORIGINAL.is_set()
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("bow by hand: {} arrows aimed by the hands ({} eye points taken from them)", ARROWS.load(Relaxed), aim::overridden());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pull_runs_from_hands_together_to_a_full_draw() {
        assert_eq!(pull(0.05), 0.0);
        assert!((pull((REST + FULL_DRAW) / 2.0) - 0.5).abs() < 1e-5);
        assert_eq!(pull(0.9), 1.0);
    }
}
