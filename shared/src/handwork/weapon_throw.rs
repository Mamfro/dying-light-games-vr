//! Throwing the melee weapon by hand (`motion_throw`, with the hand rig): the weapon flies the way
//! the right hand swung it, as hard as it swung, instead of along the look at the game's speed.
//!
//! The game throws a melee weapon as an attack (the throw button held, then its animation); at the
//! animation's release the weapon becomes a dropped item, and the dropped item's launch gives it a
//! velocity once: a fixed speed (or the player's throw power), along the player's aim, or solved
//! toward an aim point. Here, right after a launch of an item the player threw, its velocity is
//! replaced by the hand's fastest over the last [`SWING_WITHIN`] s with some part along the game's
//! own direction, at the game's speed scaled by the hand's ([`super::throwing`]'s rule). The
//! release comes some way into the animation, after the swing, so the window is long. The weapon
//! starts from where it is: in the hand.

use super::throwing::thrown;
use crate::hands;
use monaka_core::protocol::RIGHT_HAND;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One build's dropped-item launch (game DLL RVA and the dropped item's fields).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The launch `(dropped item, toss)`.
    pub launch: usize,
    /// The launch velocity (vec3).
    pub velocity: usize,
    /// Set when the item was thrown, not dropped (u8).
    pub thrown: usize,
    /// The owner: a pointer into the player at [`OWNER_PART`].
    pub owner: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The part of the player an item's owner pointer points at.
const OWNER_PART: usize = 0x100;
/// The swing's speed is the hand's fastest over this long before the launch (seconds).
const SWING_WITHIN: f32 = 0.6;

type LaunchFn = unsafe extern "C" fn(usize, u8);

static LAUNCH_ORIGINAL: Original<LaunchFn> = Original::new();
static BUILD: OnceLock<(Build, usize)> = OnceLock::new();
static THROWS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s dropped-item launch in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = BUILD.set((build, gamedll.at(build.player_vtable)));
    // SAFETY: the detour has the launch's signature (as its call sites use it); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&LAUNCH_ORIGINAL, "dropped item launch", gamedll.at(build.launch), launch as LaunchFn)? };
    Ok(())
}

/// Whether `item` was thrown by the local player.
fn players_throw(item: usize, build: &Build, player_vtable: usize) -> bool {
    if mem::read::<u8>(item + build.thrown).unwrap_or(0) == 0 {
        return false;
    }
    let Some(owner) = mem::read::<usize>(item + build.owner).filter(|&o| o > OWNER_PART) else { return false };
    mem::read::<usize>(owner - OWNER_PART) == Some(player_vtable)
}

unsafe extern "C" fn launch(item: usize, toss: u8) {
    let _flight = InFlight::enter();
    let before = BUILD.get().and_then(|(b, _)| mem::read::<[f32; 3]>(item + b.velocity));
    // SAFETY: forwards the game's own call.
    unsafe { LAUNCH_ORIGINAL.get()(item, toss) };
    let Some(&(build, player_vtable)) = BUILD.get() else { return };
    // Only a launch that just happened: the velocity was zero and now is not.
    let Some(game) = mem::read::<[f32; 3]>(item + build.velocity) else { return };
    if before != Some([0.0; 3]) || hands::length(game) < 1e-3 || !players_throw(item, &build, player_vtable) {
        return;
    }
    let Some((hand, _)) = hands::peak_velocity(RIGHT_HAND, SWING_WITHIN, Some(game)) else { return };
    let Some(throw) = thrown(hand, hands::length(game)) else { return };
    if mem::write(item + build.velocity, throw) && THROWS.fetch_add(1, Relaxed) < 40 {
        log!("weapon thrown by hand: the swing at {:.1} m/s, the game's {:.1} m/s, thrown at {:.1} m/s", hands::length(hand), hands::length(game), hands::length(throw));
    }
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("weapon thrown by hand: {} throws", THROWS.load(Relaxed));
    }
}
