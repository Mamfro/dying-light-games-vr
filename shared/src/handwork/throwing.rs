//! Throwing by hand (with the hand rig): a throwable (knife, molotov, grenade, a carried prop)
//! leaves the left hand the moment the tool button is let go, the way the left controller moved
//! through its fastest moment, as hard as it moved; a hand at rest just drops it. The look, the
//! marker and the trajectory the game shows play no part.
//!
//! The game's throw controller (`WeaponThrowController`) runs the throw as states (idle, wind-up,
//! held, three throw states, recover): letting go starts the throw animation, and the animation's
//! release event makes the thrown object. At that moment the controller asks two of its own
//! functions for the object's start point (the eye, a little ahead) and its velocity (the look
//! direction times the item's look impulse, plus up times its up impulse). Here:
//!
//! - when letting go takes the controller into a throw state, the release is made at once (the game
//!   makes it once per throw, so the animation's later event does nothing more);
//! - during that release the start point is the left hand ([`crate::hands`]: tools are the left
//!   hand's), and the velocity points the way the hand travelled through its fastest moment of the
//!   last [`RELEASE_WINDOW`] s ([`crate::hands::stroke`]), the item's own throw speed scaled by the
//!   hand's speed against [`FULL_THROW`] (up to [`MOST_SHARE`] of it), so every throwable keeps its
//!   own range for a full throw. Knives, stars and spears fly as projectiles that keep this speed;
//!   bombs and props as physics objects given it.
//!
//! The game's trajectory arc while the throwable is held (where the look's throw would land) says
//! nothing of where the hand will throw it: it is off during each of the player's throw updates
//! (the player parameter `ThrowParabolaEnabled`, put back after).
//!
//! While a throw is under way ([`throwing_now`]) the arms stay apart, each on its controller (the
//! throw's animation brings the left hand to the right), and the aim stays the head's.

use crate::hands;
use monaka_core::math;
use monaka_core::protocol::LEFT_HAND;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    /// The controller's state (i32): [`IDLE`], [`THROWING`].
    pub state: usize,
    /// Set once this throw's release has been made (u8).
    pub released: usize,
    /// The hand the game throws with (u32: 0 the right hand's holder, 1 the left's), logged.
    pub hand: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
    /// The player's vtable slot that returns its live parameter block, and in it
    /// `ThrowParabolaEnabled` (u8).
    pub parameters_slot: usize,
    pub arc: usize,
}

/// Every controller's player (the controllers' own eye and look read it there).
const PLAYER: usize = 0x08;
/// The controller's idle state, and the states of a throw under way (after the button is let go).
const IDLE: i32 = -1;
const THROWING: std::ops::RangeInclusive<i32> = 2..=4;
/// The throw's speed is the hand's fastest over this long before the release (seconds)...
const RELEASE_WINDOW: f32 = 0.5;
/// ...and its way the hand's from this long before that fastest moment to this long after.
const STROKE_BEFORE: f32 = 0.1;
const STROKE_AFTER: f32 = 0.04;
/// A hand moving this fast (m/s) throws with the item's full speed (a throw in a room is slower
/// than the arm's in the game)...
const FULL_THROW: f32 = 5.0;
/// ...and at most this share of it.
const MOST_SHARE: f32 = 1.25;
/// Below this (m/s) the hand is at rest: what it held drops, at [`DROP_SPEED`] straight down.
const STILL: f32 = 0.05;
const DROP_SPEED: f32 = 0.5;
const DOWN: [f32; 3] = [0.0, -1.0, 0.0];
/// The object starts this far ahead of the hand along its flight (metres), clear of the fingers.
const AHEAD_OF_HAND: f32 = 0.1;
/// The hand's place must be this recent (seconds).
const FRESH: f32 = 0.1;
/// A throw counts as under way for at most this long (one cut short, the tool put away, may never
/// say it ended), and this long after it ended (the game then poses the next tool).
const LONGEST_THROW: Duration = Duration::from_secs(6);
const AFTER_THROW: Duration = Duration::from_millis(1500);

/// When a player's throw controller left idle (while it is out of it) and when it last went back
/// to it: whether a throw is under way, for any game's throw hooks.
pub struct ThrowSpan {
    started: Mutex<Option<Instant>>,
    ended: Mutex<Option<Instant>>,
}

impl ThrowSpan {
    pub const fn new() -> Self {
        Self { started: Mutex::new(None), ended: Mutex::new(None) }
    }

    /// The controller is out of idle (`busy`), or back in it.
    pub fn note(&self, busy: bool) {
        let Ok(mut started) = self.started.lock() else { return };
        if !busy
            && started.is_some()
            && let Ok(mut ended) = self.ended.lock()
        {
            *ended = Some(Instant::now());
        }
        *started = match (busy, *started) {
            (false, _) => None,
            (true, Some(at)) => Some(at),
            (true, None) => Some(Instant::now()),
        };
    }

    /// Under way (winding up to recovering, for at most [`LONGEST_THROW`]), or ended less than
    /// [`AFTER_THROW`] ago.
    pub fn now(&self) -> bool {
        let fresh = |at: &Mutex<Option<Instant>>, within: Duration| at.lock().ok().and_then(|s| *s).is_some_and(|at| at.elapsed() < within);
        fresh(&self.started, LONGEST_THROW) || fresh(&self.ended, AFTER_THROW)
    }
}

impl Default for ThrowSpan {
    fn default() -> Self {
        Self::new()
    }
}

/// A throw's velocity from the throwing hand: as fast as its `fastest` lately, the way it
/// travelled through that moment (its `stroke`: a short flick stops before the trigger is let go,
/// and what comes after is not the throw), else the way of its fastest, else down.
pub fn throw_velocity(fastest: [f32; 3], stroke: Option<[f32; 3]>) -> [f32; 3] {
    let speed = math::length(fastest);
    let way = stroke.unwrap_or(if speed > 0.0 { fastest.map(|c| c / speed) } else { DOWN });
    way.map(|c| c * speed)
}

type UpdateFn = unsafe extern "C" fn(usize, f32);
type VectorFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *mut [f32; 3];
type ReleaseFn = unsafe extern "C" fn(usize);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static VELOCITY_ORIGINAL: Original<VectorFn> = Original::new();
static START_ORIGINAL: Original<VectorFn> = Original::new();

/// The build, and the release and player vtable relocated.
static BUILD: OnceLock<(Build, usize, usize)> = OnceLock::new();
static THROWS: AtomicU64 = AtomicU64::new(0);
static DROPS: AtomicU64 = AtomicU64::new(0);
static SPAN: ThrowSpan = ThrowSpan::new();

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

/// Whether the player's throw is under way (winding up to recovering), or ended less than
/// [`AFTER_THROW`] ago.
pub fn throwing_now() -> bool {
    BUILD.get().is_some() && SPAN.now()
}

/// The left hand's throw now: where it is and its velocity (world), as fast as its fastest lately,
/// the way it travelled through that moment; none without the hand tracked.
fn throwing_hand() -> Option<([f32; 3], [f32; 3])> {
    let at = hands::position(&hands::palm(LEFT_HAND, FRESH)?);
    let fastest = hands::fastest(LEFT_HAND, RELEASE_WINDOW).unwrap_or([0.0; 3]);
    Some((at, throw_velocity(fastest, hands::stroke(LEFT_HAND, RELEASE_WINDOW, STROKE_BEFORE, STROKE_AFTER))))
}

/// The player's `ThrowParabolaEnabled` flag (its address) behind `controller`, the player's.
fn arc_flag(controller: usize, build: &Build) -> Option<usize> {
    let player = mem::read::<usize>(controller + PLAYER)?;
    let getter = mem::read::<usize>(player).and_then(|vtable| mem::read::<usize>(vtable + build.parameters_slot)).filter(|&g| g != 0)?;
    // SAFETY: the player's own parameter getter, no arguments beyond it, on the game thread, as the
    // throw update itself calls it.
    let parameters = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(player) };
    (parameters != 0).then_some(parameters + build.arc)
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    let Some(&(build, release, player_vtable)) = BUILD.get() else {
        // SAFETY: forwards the game's own call.
        return unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    };
    if !players(controller, player_vtable) {
        // SAFETY: forwards the game's own call.
        return unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    }
    // The arc off for this update only.
    let arc = arc_flag(controller, &build).and_then(|flag| mem::read::<u8>(flag).map(|was| (flag, was)));
    if let Some((flag, _)) = arc {
        mem::write::<u8>(flag, 0);
    }
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    if let Some((flag, was)) = arc {
        mem::write::<u8>(flag, was);
    }
    let state = mem::read::<i32>(controller + build.state).unwrap_or(IDLE);
    SPAN.note(state != IDLE);
    let released = mem::read::<u8>(controller + build.released).unwrap_or(1) != 0;
    if !THROWING.contains(&state) || released {
        return;
    }
    let Some((at, velocity)) = throwing_hand() else { return };
    if THROWS.load(Relaxed) + DROPS.load(Relaxed) < 40 {
        let game_hand = if mem::read::<u32>(controller + build.hand) == Some(1) { "left" } else { "right" };
        log!("throw by hand: released from the left hand (the game's throwing hand: the {game_hand}), {:.1} m/s", math::length(velocity));
    }
    RELEASING.with(|r| r.set(Some((at, velocity))));
    // SAFETY: the game's own release on the controller whose update just ran, on the thread that
    // runs it, as the update itself makes it on the release event.
    unsafe { std::mem::transmute::<usize, ReleaseFn>(release)(controller) };
    RELEASING.with(|r| r.set(None));
}

/// `hand` (m/s) as a throw at the item's own `full` speed (m/s): the hand's direction, the speed's
/// share by how hard the hand moved; a hand at rest drops it. None for an item with no speed.
pub(crate) fn thrown(hand: [f32; 3], full: f32) -> Option<[f32; 3]> {
    let speed = math::length(hand);
    if !(full.is_finite() && full > 1e-3 && speed.is_finite()) {
        return None;
    }
    if speed < STILL {
        return Some(DOWN.map(|c| c * DROP_SPEED));
    }
    let share = (speed / FULL_THROW).min(MOST_SHARE);
    Some(hand.map(|v| v / speed * full * share))
}

unsafe extern "C" fn velocity(controller: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call: the item's own velocity along the look.
    let result = unsafe { VELOCITY_ORIGINAL.get()(controller, out) };
    let Some((_, hand)) = RELEASING.with(Cell::get) else { return result };
    // SAFETY: the caller's out vector, just written by the original.
    let Some(game) = (unsafe { result.as_mut() }) else { return result };
    let full = math::length(*game);
    let Some(throw) = thrown(hand, full) else { return result };
    let still = math::length(hand) < STILL;
    let n = if still { DROPS.fetch_add(1, Relaxed) } else { THROWS.fetch_add(1, Relaxed) };
    if n < 40 {
        if still {
            log!("throw by hand: the hand at rest: dropped");
        } else {
            log!("throw by hand: the hand at {:.1} m/s, the item's {full:.1} m/s, thrown at {:.1} m/s along {:.2?}", math::length(hand), math::length(throw), throw.map(|c| c / math::length(throw)));
        }
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
    let speed = math::length(hand);
    let ahead = if speed >= STILL { hand.map(|v| v / speed * AHEAD_OF_HAND) } else { [0.0; 3] };
    *point = [0, 1, 2].map(|k| at[k] + ahead[k]);
    result
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("throw by hand: {} throws, {} drops", THROWS.load(Relaxed), DROPS.load(Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hard_throw_keeps_the_items_range_and_a_still_hand_drops() {
        let full = thrown([0.0, 0.0, -FULL_THROW], 20.0).unwrap();
        assert!((full[2] + 20.0).abs() < 1e-4, "{full:?}");
        let soft = thrown([0.0, 0.0, -1.0], 20.0).unwrap();
        assert!((soft[2] + 20.0 / FULL_THROW).abs() < 1e-4, "as hard as the hand: {soft:?}");
        let hardest = thrown([0.0, 0.0, -50.0], 20.0).unwrap();
        assert!((hardest[2] + 20.0 * MOST_SHARE).abs() < 1e-4, "{hardest:?}");
        assert_eq!(thrown([0.0; 3], 20.0), Some(DOWN.map(|c| c * DROP_SPEED)), "no motion is no throw");
        assert!(thrown([0.0, 0.0, -5.0], 0.0).is_none());
    }
}
