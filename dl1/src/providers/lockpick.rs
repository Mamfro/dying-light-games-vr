//! Provider: lockpicking (`monaka_body::lockpick::Lockpicks`: Dying Light 1's twist rules, its
//! hands, whether a lock is being picked), with the minigame's analog actions taken the game's own
//! way (`monaka_pad::actions::Actions::engine` for `Action::Pick` and `Action::Turn`, through
//! [`crate::providers::actions`]). Its component is `crate::LOCKPICK`.
//!
//! The minigame reads its controls through one function ([`read`]): the pick is the direction of
//! four actions (up, down, left, right; the left stick), which it turns into an angle and moves the
//! pick toward; the screwdriver is the right stick's left and right. Each read is answered from
//! the analog action followed: the pick's angle (`Action::Pick`, from straight up, as far as a
//! quarter turn either way) as the four directions, the turn (`Action::Turn`) as the screwdriver's
//! left or right. A read without a value ready stays the stick's, and a stick pushed wins over the
//! hands for a while. A lock is being picked from the player's lockpick controller's start to its
//! stop ([`start`], [`stop`]); in each, the first read of each action is fresh.

use crate::Dl1;
use crate::providers::skeleton;
use monaka_body::hands::{At, Point};
use monaka_body::lockpick::{Lockpicking, Lockpicks, Twist};
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_pad::actions::{self, Action, Reading};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

/// Each hand's roll about where it points, from its orientation as the lock began: the pick turns
/// as far as the left hand, the screwdriver fully at 0.8 radians of the right hand.
impl Lockpicks for Dl1 {
    /// The controller's aim pose as sampled with the frame (`monaka_body::hands`).
    const LOCKPICKING: Lockpicking = Lockpicking { twist: Twist::FromStart, pick_gain: 1.0, full_turn: 0.8, hand: (Point::Controller, At::Frame) };

    fn picking() -> bool {
        picking()
    }

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        install(start.hooks, &start.build.gamedll)?;
        Ok(())
    }

    fn report() {
        report();
    }
}

/// The minigame's reading of an action: `(minigame, action) -> value`.
const READ: usize = 0x782e60;
/// The pick's directions (the left stick) and the screwdriver's left and right (the right stick).
const PICK_UP: u32 = 0xc5;
const PICK_DOWN: u32 = 0xc6;
const PICK_LEFT: u32 = 0xc7;
const PICK_RIGHT: u32 = 0xc8;
const DRIVER_LEFT: u32 = 0xc9;
const DRIVER_RIGHT: u32 = 0xca;
/// Every action the minigame reads.
pub(crate) const READS_OF: [u32; 6] = [PICK_UP, PICK_DOWN, PICK_LEFT, PICK_RIGHT, DRIVER_LEFT, DRIVER_RIGHT];
/// `LockpickController`'s start and stop (its vtable slots 18 and 19): `(controller)`. The start
/// marks the controller running in its player's table of running controllers.
const START: usize = 0xd3f170;
const STOP: usize = 0xd3f5e0;
/// A stick pushed this far wins, and keeps winning this long after.
const STICK: f32 = 0.2;
const STICK_HOLD: Duration = Duration::from_millis(400);

type ReadFn = unsafe extern "C" fn(*mut c_void, u32) -> f32;
type ControllerFn = unsafe extern "C" fn(*mut c_void);

static READ_ORIGINAL: Original<ReadFn> = Original::new();
static START_ORIGINAL: Original<ControllerFn> = Original::new();
static STOP_ORIGINAL: Original<ControllerFn> = Original::new();

/// The minigame under way.
struct Game {
    /// Whether the pick's and the turn's next read is their first in this minigame.
    fresh: [bool; 2],
    /// Until when a stick wins.
    stick_until: Instant,
}

static GAME: Mutex<Option<Game>> = Mutex::new(None);
static READS: AtomicU64 = AtomicU64::new(0);
static GAMES: AtomicU64 = AtomicU64::new(0);

/// Whether a lock is being picked: the player's lockpick controller started and has not stopped.
pub fn picking() -> bool {
    GAME.locked().is_some()
}

/// Hooks the minigame's reads and the lockpick controller's start and stop (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    if READ_ORIGINAL.is_set() {
        return Ok(());
    }
    // SAFETY: each detour has its function's signature (read from its code and its callers); the
    // game DLL's build is checked by the caller; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&START_ORIGINAL, "lockpick controller start", gamedll.at(START), start as ControllerFn)?;
        hooks.inline_decoded(&STOP_ORIGINAL, "lockpick controller stop", gamedll.at(STOP), stop as ControllerFn)?;
        hooks.inline_decoded(&READ_ORIGINAL, "lockpicking input", gamedll.at(READ), read as ReadFn)?;
    }
    Ok(())
}

/// The lockpick controller's start: the player's begins a minigame.
unsafe extern "C" fn start(controller: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { START_ORIGINAL.get()(controller) };
    if skeleton::players(controller as usize) {
        let n = GAMES.fetch_add(1, Relaxed) + 1;
        *GAME.locked() = Some(Game { fresh: [true; 2], stick_until: Instant::now() });
        log!("lockpicking by hand: minigame {n} started");
    }
}

/// The lockpick controller's stop: the player's minigame is over.
unsafe extern "C" fn stop(controller: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { STOP_ORIGINAL.get()(controller) };
    if skeleton::players(controller as usize) && GAME.locked().take().is_some() {
        log!("lockpicking by hand: minigame stopped");
    }
}

/// Whether the minigame's reads are hooked: the pick and the turn are then the game's own way.
pub fn installed() -> bool {
    READ_ORIGINAL.is_set()
}

/// The analog action the minigame's action `action` reads, if any.
pub(crate) fn followed(action: u32) -> Option<Action> {
    // The pick's four directions first, then the screwdriver's two.
    let at = READS_OF.iter().position(|&read| read == action)?;
    Some(if at < 4 { Action::Pick } else { Action::Turn })
}

/// The value of the minigame's action `action` for the pick's angle or the turn followed (`value`).
pub(crate) fn answer(action: u32, value: f32) -> f32 {
    if matches!(action, PICK_UP | PICK_DOWN | PICK_LEFT | PICK_RIGHT) {
        let angle = value.clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
        let (up, right) = (angle.cos(), angle.sin());
        match action {
            PICK_UP => up.max(0.0),
            PICK_DOWN => (-up).max(0.0),
            PICK_RIGHT => right.max(0.0),
            _ => (-right).max(0.0),
        }
    } else {
        let amount = value.clamp(-1.0, 1.0);
        if action == DRIVER_RIGHT { amount.max(0.0) } else { (-amount).max(0.0) }
    }
}

unsafe extern "C" fn read(minigame: *mut c_void, action: u32) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let stick = unsafe { READ_ORIGINAL.get()(minigame, action) };
    let Some(followed) = followed(action) else { return stick };
    if !actions::ready(followed) {
        return stick;
    }
    let which = usize::from(followed == Action::Turn);
    let now = Instant::now();
    let fresh = {
        let mut game = GAME.locked();
        // No minigame of the player's started: the stick's.
        let Some(state) = game.as_mut() else { return stick };
        if stick.abs() > STICK {
            state.stick_until = now + STICK_HOLD;
        }
        if now < state.stick_until {
            return stick;
        }
        state.fresh[which]
    };
    let Some(value) = actions::value(followed, Reading { fresh }) else { return stick };
    if fresh {
        GAME.locked().iter_mut().for_each(|g| g.fresh[which] = false);
    }
    READS.fetch_add(1, Relaxed);
    answer(action, value)
}

pub fn report() {
    let games = GAMES.load(Relaxed);
    if games > 0 {
        log!("lockpicking by hand: {games} minigames, {} reads answered by the hands", READS.load(Relaxed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pick_and_the_turn_take_their_own_reads() {
        assert!(READS_OF[..4].iter().all(|&a| followed(a) == Some(Action::Pick)));
        assert!(READS_OF[4..].iter().all(|&a| followed(a) == Some(Action::Turn)));
        assert_eq!(followed(0xc4), None);
        // Straight up, the pick pushes up only; a quarter turn right, right only.
        assert_eq!((answer(PICK_UP, 0.0), answer(PICK_RIGHT, 0.0), answer(PICK_DOWN, 0.0)), (1.0, 0.0, 0.0));
        assert!((answer(PICK_RIGHT, 2.0) - 1.0).abs() < 1e-6 && answer(PICK_UP, 2.0) < 1e-6, "past a quarter turn: a quarter");
        assert_eq!((answer(DRIVER_LEFT, -0.5), answer(DRIVER_RIGHT, -0.5)), (0.5, 0.0));
    }
}

#[cfg(test)]
mod lift_checks;
