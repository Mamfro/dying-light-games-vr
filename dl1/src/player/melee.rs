//! Physical melee (`physical_melee`, with the hand rig): a swing hits where the weapon in the
//! player's hand passes through an enemy, from the way it moved. DL1's own hit detection already
//! sweeps a blade along the arms' `R_HandHolder` element frame to frame and tests it against each
//! enemy's bones, and the hand rig poses that element; three things in it are made for the canned
//! attack animation instead:
//!
//! - it sweeps only in a window the attack sets up at its start (about 290 ms after the press, for
//!   about 260 ms), by when a real swing is over: the window here opens at the attack's start;
//! - the hit's direction is the attack animation's (a fixed sideways direction per attack type),
//!   not the blade's motion: that override is cleared, so the game keeps the direction the hand
//!   moved;
//! - the blade reaches 1.3 times the weapon's range plus 0.1 m (2.4 m for a machete): here it is
//!   the length of the weapon in the hand ([`Physical::blade`]), or a fist's reach.
//!
//! A swing still presses the attack ([`monaka_arms::melee`]); the game's damage, wounds,
//! reactions, stamina and sounds are its own.

use crate::{engine, player::hands};
use monaka_arms::melee::{Held, Physical};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// Sets up an attack: `(controller, attack type)`. Writes the attack's direction and its hit
/// window, among much else.
const ATTACK_START: usize = 0xdc9320;
/// The attack types the attack start sets up (bit per type, from its own check).
const ATTACK_TYPES: u32 = 0x62ffff;
/// The blade's reach for this frame: `(controller, weapon) -> metres`, the weapon's range scaled
/// for how far the player looks up or down.
const REACH: usize = 0xdbec70;
/// The attack's direction (player-local); non-zero overrides each hit's own direction.
const ATTACK_DIRECTION: usize = 0x29c;
/// When the attack starts looking for hits, seconds after it starts.
const WINDOW_START: usize = 0x2a8;
/// How far past the reach the hit test extends the blade: `reach * 1.3 + 0.1`.
const REACH_SCALE: f32 = 1.3;
const REACH_EXTRA: f32 = 0.1;

type AttackStartFn = unsafe extern "C" fn(*mut c_void, i32);
type ReachFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> f32;
type GetterFn = unsafe extern "system" fn(*mut c_void) -> *mut c_void;

static ATTACK_START_ORIGINAL: Original<AttackStartFn> = Original::new();
static REACH_ORIGINAL: Original<ReachFn> = Original::new();

static PHYSICAL: OnceLock<Physical> = OnceLock::new();
static ATTACKS: AtomicU64 = AtomicU64::new(0);
static REACHES: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module, physical: Physical) -> Result<(), Rejection> {
    let _ = PHYSICAL.set(physical);
    // SAFETY: the detours have the targets' signatures (read from their code and call sites);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&ATTACK_START_ORIGINAL, "melee attack start", gamedll.at(ATTACK_START), attack_start as AttackStartFn)?;
        hooks.inline_decoded(&REACH_ORIGINAL, "melee reach", gamedll.at(REACH), reach as ReachFn)?;
    }
    physical.apply();
    Ok(())
}

/// The player's melee controller, with what the arms hold: the controller's player is the arms
/// model the rig poses, and the arms hold no gun.
fn player_melee(controller: *mut c_void) -> Option<Held> {
    let held = monaka_arms::melee::held().filter(|&h| h != Held::Gun)?;
    let get = mem::read::<usize>(controller as usize).and_then(|vtable| mem::read::<usize>(vtable + 0x20))?;
    if !Module::find(engine::GAMEDLL).is_some_and(|m| m.contains(get)) {
        return None;
    }
    // SAFETY: the controller's own getter for its player, on the game thread inside its update.
    let player = unsafe { std::mem::transmute::<usize, GetterFn>(get)(controller) } as usize;
    (player != 0 && player == hands::arms_model()).then_some(held)
}

unsafe extern "C" fn attack_start(controller: *mut c_void, attack: i32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { ATTACK_START_ORIGINAL.get()(controller, attack) };
    // The game returns at once for anything but these attack types.
    if !(0..=0x16).contains(&attack) || (ATTACK_TYPES >> attack) & 1 == 0 {
        return;
    }
    let Some(held) = player_melee(controller) else { return };
    let at = controller as usize;
    let window = mem::read::<f32>(at + WINDOW_START).unwrap_or(f32::NAN);
    // No window set up: nothing to open (as in DL2 and The Beast).
    if !(window >= 0.0) {
        return;
    }
    let direction = mem::read::<[f32; 3]>(at + ATTACK_DIRECTION).unwrap_or([f32::NAN; 3]);
    // The game's own fields of its own controller, written on the game thread inside its call.
    mem::write::<f32>(at + WINDOW_START, 0.0);
    mem::write::<[f32; 3]>(at + ATTACK_DIRECTION, [0.0; 3]);
    let n = ATTACKS.fetch_add(1, Relaxed);
    if n < 30 {
        log!("physical melee: attack {n} type {attack} with {held:?}: window from {window:.3} s -> 0, direction {direction:.2?} -> the blade's");
    }
}

unsafe extern "C" fn reach(controller: *mut c_void, weapon: *mut c_void) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let game = unsafe { REACH_ORIGINAL.get()(controller, weapon) };
    let (Some(held), Some(physical)) = (player_melee(controller), PHYSICAL.get()) else { return game };
    let length = physical.reach(held);
    let ours = ((length - REACH_EXTRA) / REACH_SCALE).max(0.01);
    let n = REACHES.fetch_add(1, Relaxed);
    if n < 10 || n.is_power_of_two() {
        log!("physical melee: reach {game:.2} m -> {ours:.2} m (a {length} m blade with {held:?})");
    }
    ours
}
