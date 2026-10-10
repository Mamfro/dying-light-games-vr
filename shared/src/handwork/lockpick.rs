//! Lockpicking by hand (`motion_lockpick`): twisting the left wrist turns the pick around the lock,
//! twisting the right wrist turns the screwdriver (the pad's sticks: the left moves the pick, the
//! right the screwdriver).
//!
//! The lockpick minigame has its own parts. The pick's input step turns the stick into a target
//! angle (degrees, 0 to [`MOST_ANGLE`]) that its update then moves the pick toward, holding it
//! while the screwdriver turns; the screwdriver's step reads its turn as an analog action through
//! the game's input poll. Here, after the pick's step, the target angle is the angle the pick had
//! when this lock began plus the left wrist's twist since then times [`PICK_GAIN`] (a comfortable
//! half turn of the wrist goes all the way round); and the poll answers the screwdriver's action
//! with the right wrist's twist since the lock began over [`FULL_TURN`] (either way), or the
//! stick's when that is more. The game's own sweet spot, blocking and breaking stay as they are.

use monaka_arms::controllers;
use monaka_core::math::rotate;
use monaka_core::protocol::{HandPose, LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
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
/// The pick turns this many times the wrist's twist.
const PICK_GAIN: f32 = 2.0;
/// The wrist twisted this far (degrees) turns the screwdriver fully.
const FULL_TURN: f32 = 60.0;
/// A lock not seen this long (seconds) is a new one when it comes back.
const GONE: f32 = 1.0;

type PickInputFn = unsafe extern "C" fn(usize, usize);
type PollFn = unsafe extern "C" fn(usize, u32) -> f32;

static PICK_INPUT_ORIGINAL: Original<PickInputFn> = Original::new();
static POLL_ORIGINAL: Original<PollFn> = Original::new();
static BUILD: OnceLock<Build> = OnceLock::new();
static LOCKS: AtomicU64 = AtomicU64::new(0);

/// The lock being picked: its minigame, when it was last seen, and where the pick and both wrists
/// were when it began (degrees).
#[derive(Clone, Copy)]
struct Lock {
    minigame: usize,
    seen: Instant,
    pick_start: f32,
    wrists_start: [f32; 2],
}

static LOCK: Mutex<Option<Lock>> = Mutex::new(None);

/// Hooks `build`'s pick input step and the input poll in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = BUILD.set(build);
    // SAFETY: the detours have the targets' signatures (as the minigame calls them); the game
    // DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&PICK_INPUT_ORIGINAL, "lockpick input", gamedll.at(build.pick_input), pick_input as PickInputFn)?;
        hooks.inline_decoded(&POLL_ORIGINAL, "input poll", gamedll.at(build.poll), poll as PollFn)?;
    }
    Ok(())
}

/// How far the wrist holding `palm` is twisted about where it points (degrees, clockwise as the
/// player sees it), from its knuckles' up against the world's up.
fn twist(palm: &HandPose) -> Option<f32> {
    let forward = rotate(palm.orientation, [0.0, 0.0, -1.0]);
    let up = rotate(palm.orientation, [0.0, 1.0, 0.0]);
    // The world's up, square to where the hand points (the hand pointing up or down has none).
    let level = [-forward[1] * forward[0], 1.0 - forward[1] * forward[1], -forward[1] * forward[2]];
    let length = (level[0] * level[0] + level[1] * level[1] + level[2] * level[2]).sqrt();
    if length < 0.2 {
        return None;
    }
    let level = level.map(|v| v / length);
    // Right of where the hand points: forward × level up.
    let right = [forward[1] * level[2] - forward[2] * level[1], forward[2] * level[0] - forward[0] * level[2], forward[0] * level[1] - forward[1] * level[0]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    Some(dot(up, right).atan2(dot(up, level)).to_degrees())
}

/// `angle` turned into -180..180 degrees.
fn wrapped(angle: f32) -> f32 {
    (angle + 180.0).rem_euclid(360.0) - 180.0
}

fn wrist(side: usize) -> Option<f32> {
    controllers::palm(side).filter(|p| p.valid != 0).as_ref().and_then(twist)
}

/// The lock being picked by minigame `minigame` now (begun now if it is new).
fn lock(minigame: usize, pick_now: f32) -> Option<Lock> {
    let mut guard = LOCK.lock().ok()?;
    let fresh = guard.filter(|l| l.minigame == minigame && l.seen.elapsed().as_secs_f32() < GONE);
    let mut lock = match fresh {
        Some(lock) => lock,
        None => {
            let wrists_start = [wrist(LEFT_HAND)?, wrist(RIGHT_HAND).unwrap_or(0.0)];
            if LOCKS.fetch_add(1, Relaxed) < 40 {
                log!("lockpick by hand: a lock begun, the pick at {pick_now:.0} degrees");
            }
            Lock { minigame, seen: Instant::now(), pick_start: pick_now, wrists_start }
        }
    };
    lock.seen = Instant::now();
    *guard = Some(lock);
    Some(lock)
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
    let (Some(now), Some(left)) = (mem::read::<f32>(pick + build.target), wrist(LEFT_HAND)) else { return };
    let Some(lock) = lock(minigame, now) else { return };
    let angle = (lock.pick_start + wrapped(left - lock.wrists_start[0]) * PICK_GAIN).rem_euclid(360.0).min(MOST_ANGLE);
    mem::write(pick + build.target, angle);
    mem::write(pick + build.moving, 1u8);
}

unsafe extern "C" fn poll(input: usize, action: u32) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let value = unsafe { POLL_ORIGINAL.get()(input, action) };
    if !BUILD.get().is_some_and(|b| b.screwdriver_action == action) {
        return value;
    }
    let Some(lock) = LOCK.lock().ok().and_then(|l| *l).filter(|l| l.seen.elapsed().as_secs_f32() < GONE) else { return value };
    let Some(right) = wrist(RIGHT_HAND) else { return value };
    let turn = (wrapped(right - lock.wrists_start[1]).abs() / FULL_TURN).clamp(0.0, 1.0);
    value.max(turn)
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("lockpick by hand: {} locks", LOCKS.load(Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palm(orientation: [f32; 4]) -> HandPose {
        HandPose { orientation, position: [0.0; 3], valid: 1, received_tick: 0 }
    }

    #[test]
    fn a_level_hand_is_untwisted_and_a_quarter_roll_is_ninety_degrees() {
        assert!(twist(&palm([0.0, 0.0, 0.0, 1.0])).unwrap().abs() < 1e-3);
        // A quarter turn about the hand's own axis (z), either way, reads as ±90.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let a = twist(&palm([0.0, 0.0, s, s])).unwrap();
        let b = twist(&palm([0.0, 0.0, -s, s])).unwrap();
        assert!((a.abs() - 90.0).abs() < 1e-2 && (b.abs() - 90.0).abs() < 1e-2 && a * b < 0.0, "{a} {b}");
        // Pointing straight up has no twist to read.
        assert!(twist(&palm([s, 0.0, 0.0, s])).is_none());
    }

    #[test]
    fn angles_wrap_around() {
        assert_eq!(wrapped(190.0), -170.0);
        assert_eq!(wrapped(-190.0), 170.0);
    }
}
