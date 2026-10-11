//! Reloading by hand (with the hand rig): with a gun drawn, the left hand brought to the gun (within
//! [`NEAR`] of the right controller) and taken away again (past [`AWAY`] within [`WITHIN`]) reloads,
//! as the reload button would; not again for [`COOLDOWN`]. The controllers count, not the arms: with
//! a gun held two-handed the left arm rides the gun.
//!
//! The gun's controller (`WeaponFireController`) starts a reload from its input when the reload
//! action is pressed with the gun ready (state 0): it calls the reload start with `from input` set
//! (The Beast first marks some weapons, [`Prepare`]). The start checks for itself that a reload can
//! happen (spare ammo, the clip not full). Here, after each update of the player's gun, a finished
//! gesture makes that same call; the reload then runs as the game's.

use crate::hands;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One build's gun controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The gun controller's update `(controller, dt)`.
    pub update: usize,
    /// The reload start `(controller, kind, from input)`.
    pub start: usize,
    /// The gun's state (i32): 0 ready.
    pub state: usize,
    /// What the input does before it starts a reload, where it does more than start it.
    pub prepare: Option<Prepare>,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// Before the start: the flag set when the gun's weapon (the controller's vtable slot) passes
/// either check `(weapon) -> bool`.
#[derive(Clone, Copy, Debug)]
pub struct Prepare {
    pub weapon_slot: usize,
    pub checks: [usize; 2],
    pub flag: usize,
}

/// The gun ready.
const READY: i32 = 0;
/// The left controller this near the right (metres) touches the gun...
const NEAR: f32 = 0.15;
/// ...and then this far from it within [`WITHIN`] reloads.
const AWAY: f32 = 0.25;
const WITHIN: Duration = Duration::from_millis(1000);
/// No second reload by the gesture for this long.
const COOLDOWN: Duration = Duration::from_millis(1500);
/// The hands' places must be this recent (seconds).
const FRESH: f32 = 0.1;

type UpdateFn = unsafe extern "C" fn(usize, f32);
type StartFn = unsafe extern "C" fn(usize, u8, u8);
type GetterFn = unsafe extern "C" fn(usize) -> usize;
type CheckFn = unsafe extern "C" fn(usize) -> bool;

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
/// The build, with the start, the player vtable and the prepare checks relocated.
static BUILD: OnceLock<(Build, usize, usize, Option<[usize; 2]>)> = OnceLock::new();
/// When the left hand last touched the gun, and the last reload by the gesture.
static GESTURE: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));
static RELOADS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s gun update in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let checks = build.prepare.map(|p| p.checks.map(|c| gamedll.at(c)));
    let _ = BUILD.set((build, gamedll.at(build.start), gamedll.at(build.player_vtable), checks));
    // SAFETY: the detour has the update's signature (a controller's update slot, as the engine
    // calls it); the game DLL's build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "gun update", gamedll.at(build.update), update as UpdateFn)? };
    Ok(())
}

/// The left hand came to the gun hand and has just been taken away from it.
fn gestured() -> bool {
    let (Some(left), Some(right)) = (hands::palm(LEFT_HAND, FRESH), hands::palm(RIGHT_HAND, FRESH)) else { return false };
    let (left, right) = (hands::position(&left), hands::position(&right));
    let apart = hands::length([0, 1, 2].map(|k| left[k] - right[k]));
    let Ok(mut gesture) = GESTURE.lock() else { return false };
    let now = Instant::now();
    let (touched, last) = &mut *gesture;
    if apart < NEAR {
        *touched = Some(now);
        return false;
    }
    let fresh = touched.is_some_and(|at| now.duration_since(at) < WITHIN);
    let rested = last.is_none_or(|at| now.duration_since(at) > COOLDOWN);
    if apart > AWAY && fresh && rested {
        *touched = None;
        *last = Some(now);
        return true;
    }
    false
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    let Some(&(build, start, player_vtable, checks)) = BUILD.get() else { return };
    // The gesture is read only with the gun ready, so one made while it fires is not used up.
    if !super::throwing::players(controller, player_vtable) || mem::read::<i32>(controller + build.state) != Some(READY) || !gestured() {
        return;
    }
    if let (Some(prepare), Some(checks)) = (build.prepare, checks) {
        let getter = mem::read::<usize>(controller).and_then(|vtable| mem::read::<usize>(vtable + prepare.weapon_slot)).filter(|&g| g != 0);
        // SAFETY: the controller's own weapon getter and the game's checks on that weapon, as its
        // input calls them on this thread before a reload.
        let marked = getter.is_some_and(|getter| unsafe {
            let weapon = std::mem::transmute::<usize, GetterFn>(getter)(controller);
            weapon != 0 && checks.iter().any(|&check| std::mem::transmute::<usize, CheckFn>(check)(weapon))
        });
        if marked {
            mem::write::<u8>(controller + prepare.flag, 1);
        }
    }
    let n = RELOADS.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        log!("reload by hand: reload {n} by the gesture");
    }
    // SAFETY: the game's own reload start on the player's ready gun, from its update on the game
    // thread (the update itself starts one there for an empty clip), as the reload input calls it.
    unsafe { std::mem::transmute::<usize, StartFn>(start)(controller, 0, 1) };
}

pub fn report() {
    if BUILD.get().is_some() {
        log!("reload by hand: {} reloads by the gesture", RELOADS.load(Relaxed));
    }
}
