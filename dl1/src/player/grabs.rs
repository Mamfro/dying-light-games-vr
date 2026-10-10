//! Disabled zombie grabs (`disable_zombie_grabs`, on by default): a zombie's grab never lands on
//! the player. Every grab first asks the player whether this attacker may grab it; the answer is no, as
//! the game's own `CantBeGrabbed` player property makes it. The zombie then carries on as when its
//! grab is refused for any other reason.
//!
//! In VR the reach of a grab is hard to judge (a zombie can lunge for one from 3.5 m), so grabs
//! landed far more often than on a flat screen.

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};

use crate::player::hands;

/// The player's "may this attacker grab me" check: `(player, attacker, grab kind) -> bool`.
const MAY_GRAB: usize = 0xb9e180;
/// The grab kinds the game's own `CantBeGrabbed` leaves alone (kinds 7 to 16): passed through.
const KINDS_KEPT: std::ops::Range<u32> = 7..17;

type MayGrabFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> usize;

static MAY_GRAB_ORIGINAL: Original<MayGrabFn> = Original::new();
static REFUSED: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the detour has the check's signature (read from its code: the player, the attacker,
    // the grab kind; a bool back); the game DLL's build is checked at start; the prologue is
    // decoded and moved.
    unsafe { hooks.inline_decoded(&MAY_GRAB_ORIGINAL, "player may-be-grabbed check", gamedll.at(MAY_GRAB), may_grab as MayGrabFn)? };
    log!("zombie grabs: disabled (disable_zombie_grabs=0 lets zombies grab again)");
    Ok(())
}

unsafe extern "C" fn may_grab(player: *mut c_void, attacker: *mut c_void, kind: u32) -> usize {
    let _flight = InFlight::enter();
    // Only the player the rig poses, when it is known (another player in co-op keeps the game's rule).
    let ours = hands::arms_model();
    if !KINDS_KEPT.contains(&kind) && (ours == 0 || ours == player as usize) {
        let n = REFUSED.fetch_add(1, Relaxed) + 1;
        if n <= 20 || n.is_power_of_two() {
            log!("zombie grabs: grab check {n} answered no (kind {kind}, attacker {attacker:p})");
        }
        return 0;
    }
    // SAFETY: forwards the game's own call.
    unsafe { MAY_GRAB_ORIGINAL.get()(player, attacker, kind) }
}

pub fn report() {
    let n = REFUSED.load(Relaxed);
    if n > 0 {
        log!("zombie grabs: {n} grab checks answered no");
    }
}
