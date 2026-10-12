//! Provider part: lockpicking (`monaka_arms::lockpick::Lockpicks`: the Chrome games' twist rules,
//! [`LOCKPICKING`], and whether a lock is being picked, [`picking`]), with the lockpick minigame's
//! analog actions taken the game's own way (`monaka_pad::actions::Actions::engine` for
//! `Action::Pick` and `Action::Turn`, through [`crate::providers::actions`]), as Dying Light 2 has
//! them.
//!
//! The lockpick minigame has its own parts. The pick's input step turns the stick into a target
//! angle (degrees, 0 to [`MOST_ANGLE`]) that its update then moves the pick toward, holding it
//! while the screwdriver turns; the screwdriver's step reads its turn as an analog action through
//! the game's input poll. Here, after the pick's step, the target angle is the angle the pick had
//! when this lock began plus the pick's value followed (`Action::Pick`; the first read of a lock is
//! fresh); and the poll answers the screwdriver's action with the turn followed (`Action::Turn`,
//! either way), or the stick's when that is more. A read without a value ready stays the stick's.
//! The game's own sweet spot, blocking and breaking stay as they are. A lock is being picked while
//! the pick's input step runs on a lock in play, the pick free (and for [`GONE`] after).

use crate::feature::Chrome;
use monaka_arms::hands::{At, Point};
use monaka_arms::lockpick::{Lockpicking, Twist};
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_pad::actions::{self, Action, Reading};
use monaka_producer::game::{Lack, Missing, Start};
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// One build's lockpick minigame code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The pick's input step `(pick, input block)`.
    pub pick_input: usize,
    /// The input poll `(input, action id) -> value`.
    pub poll: usize,
    /// The screwdriver's turn action.
    pub screwdriver_action: u32,
    /// The pick's minigame (pointer).
    pub minigame: usize,
    /// The minigame's state (i32); [`PLAYING`] is the lock being picked.
    pub state: usize,
    /// Set while the pick is held by the screwdriver (u8).
    pub blocked: usize,
    /// The pick's target angle (f32, degrees).
    pub target: usize,
    /// Set while the pick has input (u8).
    pub moving: usize,
}

const PLAYING: i32 = 2;
/// The pick's angles run 0 to this (degrees).
const MOST_ANGLE: f32 = 359.0;
/// A lock not seen this long (seconds) is a new one when it comes back.
const GONE: f32 = 1.0;

type PickInputFn = unsafe extern "C" fn(usize, usize);
type PollFn = unsafe extern "C" fn(usize, u32) -> f32;

static PICK_INPUT_ORIGINAL: Original<PickInputFn> = Original::new();
static POLL_ORIGINAL: Original<PollFn> = Original::new();
static BUILD: OnceLock<Build> = OnceLock::new();
static LOCKS: AtomicU64 = AtomicU64::new(0);

/// The lock being picked: its minigame, when it was last seen, and where the pick was when it
/// began (degrees).
#[derive(Clone, Copy)]
struct Lock {
    minigame: usize,
    seen: Instant,
    pick_start: f32,
}

static LOCK: Mutex<Option<Lock>> = Mutex::new(None);
/// When the pick's input step last ran on a lock in play with the pick free.
static PLAYING_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The Chrome games' twist: each wrist's roll against the world's up since the lock began; the pick
/// turns twice as far as the left wrist (a comfortable half turn of the wrist goes all the way
/// round), the screwdriver fully at 60 degrees of the right wrist; the palms read at their latest.
pub const LOCKPICKING: Lockpicking = Lockpicking { twist: Twist::AgainstUp, pick_gain: 2.0, full_turn: std::f32::consts::FRAC_PI_3, hand: (Point::Palm, At::Latest) };

/// Whether a lock is being picked: the pick's input step ran on a lock in play, the pick free,
/// less than [`GONE`] ago (a game's `Lockpicks::picking`).
pub fn picking() -> bool {
    PLAYING_AT.locked().is_some_and(|at| at.elapsed().as_secs_f32() < GONE)
}

/// Game `G`'s minigame hooked (its `Lockpicks::install`), where its build has been read for it.
pub fn install_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let build = G::FACTS.hands.and_then(|h| h.lockpick).ok_or_else(|| Lack::Missing("the lockpick is not read for this build".into()))?;
    let gamedll = crate::feature::gamedll(start)?;
    install(start.hooks, gamedll, build).missing()
}

/// Hooks `build`'s pick input step and the input poll in the game DLL (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if PICK_INPUT_ORIGINAL.is_set() {
        return Ok(());
    }
    let _ = BUILD.set(build);
    // SAFETY: the detours have the targets' signatures (as the minigame calls them); the game
    // DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&PICK_INPUT_ORIGINAL, "lockpick input", gamedll.at(build.pick_input), pick_input as PickInputFn)?;
        hooks.inline_decoded(&POLL_ORIGINAL, "input poll", gamedll.at(build.poll), poll as PollFn)?;
    }
    Ok(())
}

/// Whether the minigame is hooked: the pick and the turn are then the game's own way.
pub fn installed() -> bool {
    POLL_ORIGINAL.is_set()
}

/// The lock being picked by minigame `minigame` now (begun now if it is new, the pick at
/// `pick_now`), and whether it just began.
fn lock(minigame: usize, pick_now: f32) -> Option<(Lock, bool)> {
    let mut guard = LOCK.lock().ok()?;
    let fresh = guard.filter(|l| l.minigame == minigame && l.seen.elapsed().as_secs_f32() < GONE);
    let (mut lock, began) = match fresh {
        Some(lock) => (lock, false),
        None => {
            if LOCKS.fetch_add(1, Relaxed) < 40 {
                log!("lockpick by hand: a lock begun, the pick at {pick_now:.0} degrees");
            }
            (Lock { minigame, seen: Instant::now(), pick_start: pick_now }, true)
        }
    };
    lock.seen = Instant::now();
    *guard = Some(lock);
    Some((lock, began))
}

/// The pick's target angle (degrees) for a lock whose pick began at `start` turned by `turn`
/// (radians).
fn target(start: f32, turn: f32) -> f32 {
    (start + turn.to_degrees()).rem_euclid(360.0).min(MOST_ANGLE)
}

unsafe extern "C" fn pick_input(pick: usize, input: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call: the stick's share.
    unsafe { PICK_INPUT_ORIGINAL.get()(pick, input) };
    let Some(build) = BUILD.get() else { return };
    let Some(minigame) = mem::read::<usize>(pick + build.minigame).filter(|&m| m != 0) else { return };
    if mem::read::<i32>(minigame + build.state) != Some(PLAYING) || mem::read::<u8>(pick + build.blocked).unwrap_or(1) != 0 {
        return;
    }
    *PLAYING_AT.locked() = Some(Instant::now());
    let Some(now) = mem::read::<f32>(pick + build.target).filter(|_| actions::ready(Action::Pick)) else { return };
    let Some((lock, began)) = lock(minigame, now) else { return };
    let Some(turn) = actions::value(Action::Pick, Reading { fresh: began }) else { return };
    mem::write(pick + build.target, target(lock.pick_start, turn));
    mem::write(pick + build.moving, 1u8);
}

unsafe extern "C" fn poll(input: usize, action: u32) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let value = unsafe { POLL_ORIGINAL.get()(input, action) };
    if BUILD.get().is_none_or(|b| b.screwdriver_action != action) {
        return value;
    }
    let lock = LOCK.lock().ok().and_then(|l| *l).filter(|l| l.seen.elapsed().as_secs_f32() < GONE);
    if lock.is_none() || !actions::ready(Action::Turn) {
        return value;
    }
    actions::value(Action::Turn, Reading::default()).map_or(value, |turn| value.max(turn.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::math::wrap_degrees;

    #[test]
    fn the_target_is_the_one_worked_out_before() {
        // Before: the pick's start plus the left wrist's twist since the lock began times two,
        // in degrees; now the twist arrives in radians.
        for start in [0.0, 37.5, 200.0, 358.9] {
            for (left, began) in [(10.0f32, 10.0), (170.0, -170.0), (-120.0, 45.0), (89.9, -0.1), (179.0, 179.5)] {
                let before = (start + wrap_degrees(left - began) * 2.0).rem_euclid(360.0).min(MOST_ANGLE);
                let now = target(start, (wrap_degrees(left - began) * 2.0).to_radians());
                let apart = (before - now).abs().min(360.0 - (before - now).abs());
                assert!(apart < 1e-3, "{start} {left} {began}: {before} {now}");
            }
        }
    }
}

#[cfg(test)]
mod lift_checks;
