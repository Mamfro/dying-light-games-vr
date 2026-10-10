//! Lockpicking by hand (`lockpick_by_hand`, with the hand rig): in the lockpicking minigame, the
//! left hand's twist turns the pick and the right hand's twist works the screwdriver, instead of
//! the sticks.
//!
//! The minigame reads its controls through one function ([`read`]): the pick is the direction
//! of four actions (up, down, left, right; the left stick), which it turns into an angle and
//! moves the pick toward; the screwdriver is the right stick's left and right. Each is answered
//! from the controllers: the left hand's twist since the minigame opened (its roll about where it
//! points) as the pick's direction, the right hand's as the screwdriver's left or right. A stick
//! pushed wins over the hands.

use crate::view::stereo;
use monaka_core::math::{Quat, forward_axis, right_axis};
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

/// The minigame's reading of an action: `(minigame, action) -> value`.
const READ: usize = 0x782e60;
/// The pick's directions (the left stick) and the screwdriver's left and right (the right stick).
const PICK_UP: u32 = 0xc5;
const PICK_DOWN: u32 = 0xc6;
const PICK_LEFT: u32 = 0xc7;
const PICK_RIGHT: u32 = 0xc8;
const DRIVER_LEFT: u32 = 0xc9;
const DRIVER_RIGHT: u32 = 0xca;
/// The pick's angle per radian of twist, and the screwdriver's full turn (radians of twist).
const PICK_GAIN: f32 = 1.0;
const DRIVER_FULL: f32 = 0.8;
/// No read for this long: the next is a new minigame (the twists start again from there).
const NEW_GAME: Duration = Duration::from_millis(500);
/// A stick pushed this far wins, and keeps winning this long after.
const STICK: f32 = 0.2;
const STICK_HOLD: Duration = Duration::from_millis(400);

type ReadFn = unsafe extern "C" fn(*mut c_void, u32) -> f32;

static READ_ORIGINAL: Original<ReadFn> = Original::new();

struct Game {
    last_read: Instant,
    /// Each hand's orientation as the minigame opened.
    reference: [Option<Quat>; 2],
    /// Until when a stick wins.
    stick_until: Instant,
}

static GAME: Mutex<Option<Game>> = Mutex::new(None);
static READS: AtomicU64 = AtomicU64::new(0);
static GAMES: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the detour has the function's signature (read from its code and its callers); the
    // game DLL's build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&READ_ORIGINAL, "lockpicking input", gamedll.at(READ), read as ReadFn)? };
    log!("lockpicking by hand: the left hand's twist turns the pick, the right hand's the screwdriver");
    Ok(())
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// How far `now` is rolled from `reference` about where `now` points (radians, positive clockwise
/// as the hand sees it).
fn twist(reference: Quat, now: Quat) -> f32 {
    let forward = forward_axis(now);
    let flat = |v: [f32; 3]| {
        let d = dot(v, forward);
        [v[0] - forward[0] * d, v[1] - forward[1] * d, v[2] - forward[2] * d]
    };
    let (from, to) = (flat(right_axis(reference)), flat(right_axis(now)));
    -dot(cross(from, to), forward).atan2(dot(from, to))
}

unsafe extern "C" fn read(minigame: *mut c_void, action: u32) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let stick = unsafe { READ_ORIGINAL.get()(minigame, action) };
    let pick = matches!(action, PICK_UP | PICK_DOWN | PICK_LEFT | PICK_RIGHT);
    let driver = matches!(action, DRIVER_LEFT | DRIVER_RIGHT);
    if !(pick || driver) {
        return stick;
    }
    let side = if pick { LEFT_HAND } else { RIGHT_HAND };
    let Some(hand) = stereo::pending_hand_of(side) else { return stick };
    let now = Instant::now();
    let Ok(mut game) = GAME.lock() else { return stick };
    if game.as_ref().is_none_or(|g| now.duration_since(g.last_read) > NEW_GAME) {
        GAMES.fetch_add(1, Relaxed);
        *game = Some(Game { last_read: now, reference: [None, None], stick_until: now });
    }
    let Some(state) = game.as_mut() else { return stick };
    state.last_read = now;
    if stick.abs() > STICK {
        state.stick_until = now + STICK_HOLD;
    }
    if now < state.stick_until {
        return stick;
    }
    let reference = *state.reference[side].get_or_insert(hand.orientation);
    let turn = twist(reference, hand.orientation);
    READS.fetch_add(1, Relaxed);
    if pick {
        let angle = (turn * PICK_GAIN).clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
        let (up, right) = (angle.cos(), angle.sin());
        match action {
            PICK_UP => up.max(0.0),
            PICK_DOWN => (-up).max(0.0),
            PICK_RIGHT => right.max(0.0),
            _ => (-right).max(0.0),
        }
    } else {
        let amount = (turn / DRIVER_FULL).clamp(-1.0, 1.0);
        if action == DRIVER_RIGHT { amount.max(0.0) } else { (-amount).max(0.0) }
    }
}

pub fn report() {
    let games = GAMES.load(Relaxed);
    if games > 0 {
        log!("lockpicking by hand: {games} minigames, {} reads answered by the hands", READS.load(Relaxed));
    }
}
