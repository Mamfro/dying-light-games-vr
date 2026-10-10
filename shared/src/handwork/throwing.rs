//! Throwing by hand (`motion_throw`, with the hand rig): a throwable (knife, molotov, grenade, a
//! carried prop) leaves the hand the way the hand moved, as hard as it moved, when the tool button
//! is let go, instead of from the eye along the look a moment later.
//!
//! The game's throw controller (`WeaponThrowController`) runs the throw as states: holding the
//! button winds up and then holds the throwable ready, letting go starts the throw animation, and
//! the animation's release event makes the thrown object. At that moment the controller asks two of
//! its own functions for the object's start point (the eye, a little ahead) and its velocity (the
//! look direction times the item's look impulse, plus up times its up impulse). Here:
//!
//! - when letting go takes the controller into a throw state, the release is made at once (the game
//!   makes it once per throw, so the animation's later event does nothing more);
//! - during that release the start point is the throwing hand ([`crate::hands`]; the controller
//!   names the hand), and the velocity points the way the hand moved fastest over the last
//!   [`THROW_WITHIN`] s, with the item's own throw speed scaled by the hand's speed against
//!   [`FULL_THROW`] (between [`LEAST_SHARE`] and [`MOST_SHARE`] of it), so every throwable keeps its
//!   own range for a full throw. Knives, stars and spears fly as projectiles that keep this speed;
//!   bombs and props as physics objects given it.
//!
//! The arc the game shows while holding stays its own (along the look).

use crate::hands;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One build's throw controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The controller's update `(controller, dt)`.
    pub update: usize,
    /// The thrown object's velocity `(controller, out vec3) -> out`.
    pub velocity: usize,
    /// The thrown object's start point `(controller, out vec3) -> out`.
    pub start: usize,
    /// The release `(controller)`: makes the thrown object, once per throw.
    pub release: usize,
    /// The controller's state (i32); [`THROWING`] are the throw states.
    pub state: usize,
    /// Set once this throw's release has been made (u8).
    pub released: usize,
    /// The throwing hand (u32: 0 the right hand's holder, 1 the left's).
    pub hand: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The controller's player.
const PLAYER: usize = 0x08;
/// The controller states of a throw under way (after the button is let go).
const THROWING: std::ops::RangeInclusive<i32> = 2..=4;
/// The throw's speed is the hand's fastest over this long before the release (seconds).
pub const THROW_WITHIN: f32 = 0.15;
/// A hand moving this fast (m/s) throws with the item's full speed...
const FULL_THROW: f32 = 8.0;
/// ...and the share of it is kept between these.
const LEAST_SHARE: f32 = 0.15;
const MOST_SHARE: f32 = 1.25;
/// The object starts this far ahead of the hand along its flight (metres), clear of the fingers.
const AHEAD_OF_HAND: f32 = 0.1;
/// The hand's place must be this recent (seconds).
const FRESH: f32 = 0.1;

type UpdateFn = unsafe extern "C" fn(usize, f32);
type VectorFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *mut [f32; 3];
type ReleaseFn = unsafe extern "C" fn(usize);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static VELOCITY_ORIGINAL: Original<VectorFn> = Original::new();
static START_ORIGINAL: Original<VectorFn> = Original::new();

/// The build, and the release and player vtable relocated.
static BUILD: OnceLock<(Build, usize, usize)> = OnceLock::new();
static THROWS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The throw being released by us now: the hand's start point and velocity (world).
    static RELEASING: Cell<Option<([f32; 3], [f32; 3])>> = const { Cell::new(None) };
}

/// Hooks `build`'s throw update, velocity and start point in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = BUILD.set((build, gamedll.at(build.release), gamedll.at(build.player_vtable)));
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&UPDATE_ORIGINAL, "throw update", gamedll.at(build.update), update as UpdateFn)?;
        hooks.inline_decoded(&VELOCITY_ORIGINAL, "throw velocity", gamedll.at(build.velocity), velocity as VectorFn)?;
        hooks.inline_decoded(&START_ORIGINAL, "throw start point", gamedll.at(build.start), start as VectorFn)?;
    }
    Ok(())
}

/// Whether `controller` is the local player's (its player is a `PlayerDI_PH`).
pub(crate) fn players(controller: usize, player_vtable: usize) -> bool {
    mem::read::<usize>(controller + PLAYER).filter(|&p| p != 0).and_then(mem::read::<usize>) == Some(player_vtable)
}

/// The controller's throwing hand, as the headset's side.
fn side(controller: usize, build: &Build) -> usize {
    if mem::read::<u32>(controller + build.hand) == Some(1) { LEFT_HAND } else { RIGHT_HAND }
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    let Some(&(build, release, player_vtable)) = BUILD.get() else { return };
    let state = mem::read::<i32>(controller + build.state).unwrap_or(-1);
    let released = mem::read::<u8>(controller + build.released).unwrap_or(1) != 0;
    if !THROWING.contains(&state) || released || !players(controller, player_vtable) {
        return;
    }
    let side = side(controller, &build);
    // The throw goes some way along the look (the item's own velocity, asked for its direction).
    let mut look = [0.0f32; 3];
    // SAFETY: the game's own velocity function on its controller, as the release calls it.
    unsafe { VELOCITY_ORIGINAL.get()(controller, &mut look) };
    let toward = (hands::length(look) > 1e-3).then_some(look);
    let (Some((velocity, _)), Some(palm)) = (hands::peak_velocity(side, THROW_WITHIN, toward), hands::palm(side, FRESH)) else { return };
    if THROWS.load(Relaxed) < 40 {
        log!("throw by hand: released from the {} hand", if side == LEFT_HAND { "left" } else { "right" });
    }
    RELEASING.with(|r| r.set(Some((hands::position(&palm), velocity))));
    // SAFETY: the game's own release on the controller whose update just ran, on the thread that
    // runs it, as the update itself makes it on the release event.
    unsafe { std::mem::transmute::<usize, ReleaseFn>(release)(controller) };
    RELEASING.with(|r| r.set(None));
}

/// `hand` (m/s) as a throw at the item's own `full` speed (m/s): the hand's direction, the speed's
/// share by how hard the hand moved.
pub(crate) fn thrown(hand: [f32; 3], full: f32) -> Option<[f32; 3]> {
    let speed = hands::length(hand);
    if !(full.is_finite() && speed.is_finite() && speed > 1e-3) {
        return None;
    }
    let share = (speed / FULL_THROW).clamp(LEAST_SHARE, MOST_SHARE);
    Some(hand.map(|v| v / speed * full * share))
}

unsafe extern "C" fn velocity(controller: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call: the item's own velocity along the look.
    let result = unsafe { VELOCITY_ORIGINAL.get()(controller, out) };
    let Some((_, hand)) = RELEASING.with(Cell::get) else { return result };
    // SAFETY: the caller's out vector, just written by the original.
    let Some(game) = (unsafe { result.as_mut() }) else { return result };
    let full = hands::length(*game);
    let Some(throw) = thrown(hand, full) else { return result };
    let n = THROWS.fetch_add(1, Relaxed);
    if n < 40 {
        log!("throw by hand: the hand at {:.1} m/s, the item's {full:.1} m/s, thrown at {:.1} m/s", hands::length(hand), hands::length(throw));
    }
    *game = throw;
    result
}

unsafe extern "C" fn start(controller: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let result = unsafe { START_ORIGINAL.get()(controller, out) };
    let Some((at, hand)) = RELEASING.with(Cell::get) else { return result };
    // SAFETY: the caller's out vector, just written by the original.
    let Some(point) = (unsafe { result.as_mut() }) else { return result };
    let speed = hands::length(hand);
    let ahead = if speed > 1e-3 { hand.map(|v| v / speed * AHEAD_OF_HAND) } else { [0.0; 3] };
    *point = [0, 1, 2].map(|k| at[k] + ahead[k]);
    result
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("throw by hand: {} throws", THROWS.load(Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hard_throw_keeps_the_items_range_and_a_flick_still_flies() {
        let full = thrown([0.0, 0.0, -8.0], 20.0).unwrap();
        assert!((full[2] + 20.0).abs() < 1e-4, "{full:?}");
        let flick = thrown([0.0, 0.0, -0.5], 20.0).unwrap();
        assert!((flick[2] + 20.0 * LEAST_SHARE).abs() < 1e-4, "{flick:?}");
        assert!(thrown([0.0; 3], 20.0).is_none());
    }
}
