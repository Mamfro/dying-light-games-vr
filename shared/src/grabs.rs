//! Disabled zombie grabs (`disable_zombie_grabs`, on by default): the biters' front grabs never
//! land on the player.
//!
//! A grab is a sync action (`sync_actions_grabs.def`); the biters' front grabs (A, B, C and the
//! one shared by several biters) each require the player variable `InfectedGrabBlocked` to be
//! false, the variable the game's own grab-blocking upgrade sets. Here it is held true, with
//! `CantBeGrabbed` beside it, in the player's live parameter block, each update the producer sees
//! the player (the block is rebuilt when buffs change). Grabs that do not ask (falling onto a biter,
//! volatiles, crawlers) still happen.
//!
//! In VR the reach of a grab is hard to judge (a biter lunges for one from a few metres away), so
//! grabs land far more often than on a flat screen.

use monaka_hook::mem;
use monaka_producer::log;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// One build's player parameters (offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The player's vtable slot that returns its live parameter block.
    pub parameters_slot: usize,
    /// The flags held true in it (bool parameters): `InfectedGrabBlocked`, `CantBeGrabbed`.
    pub flags: [usize; 2],
}

static ON: AtomicBool = AtomicBool::new(false);
static SET: AtomicU64 = AtomicU64::new(0);

/// Turns the hold on for the run.
pub fn enable() {
    ON.store(true, Relaxed);
    log!("zombie grabs: biters' front grabs disabled (disable_zombie_grabs=0 lets them grab again)");
}

/// Holds the grab flags true on `player` (the local player character), from the game thread.
pub fn hold(player: usize, build: &Build) {
    if !ON.load(Relaxed) {
        return;
    }
    let Some(getter) = mem::read::<usize>(player).and_then(|vtable| mem::read::<usize>(vtable + build.parameters_slot)).filter(|&g| g != 0) else { return };
    // SAFETY: the player's own parameter getter, no arguments beyond it, called on the game thread
    // as the game's controllers call it.
    let parameters = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(player) };
    if parameters == 0 {
        return;
    }
    for flag in build.flags {
        if mem::read::<u8>(parameters + flag) == Some(0) && mem::write(parameters + flag, 1u8) {
            let n = SET.fetch_add(1, Relaxed) + 1;
            if n <= 4 || n.is_power_of_two() {
                log!("zombie grabs: grab block set ({n})");
            }
        }
    }
}

pub fn report() {
    if ON.load(Relaxed) {
        log!("zombie grabs: the grab block was set {} times", SET.load(Relaxed));
    }
}
