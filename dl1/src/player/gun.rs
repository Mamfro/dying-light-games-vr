//! Shooting from the barrel (`shoot_from_barrel`, with the hand rig): a gun's shots leave its
//! muzzle along its barrel, where the gun in the hand points, instead of from the eye along the
//! game's aim. Each pellet keeps its spread around the aim, turned onto the barrel.
//!
//! The fire controller's shot ([`shoot`]) makes a projectile per pellet from the eye along the aim
//! with its spread; they are placed by the hands ([`projectile`]). The rig holds a gun so its
//! holder (`R_HandHolder`) points along the hand's aim with its +z: that is the barrel, and the
//! muzzle is [`MUZZLE`] along it. A shot at a given target point keeps the game's aim.

use crate::player::{bow, hand_world, hands, projectile};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// `WeaponFireController`'s shot: `(controller, target point or a zero vector) -> fired`.
const SHOOT: usize = 0xda0160;
/// The player's aim direction (its vfunc +0x748): `(player, out) -> out`.
const AIM_SLOT: usize = 0x748;
/// From the gun holder to the muzzle, along the barrel (metres).
const MUZZLE: f32 = 0.25;

type ShootFn = unsafe extern "C" fn(*mut c_void, *const f32) -> usize;
type AimFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *const [f32; 3];

static SHOOT_ORIGINAL: Original<ShootFn> = Original::new();
static SHOTS: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the detour has the function's signature (read from its code and its callers); the
    // game DLL's build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&SHOOT_ORIGINAL, "gun shot", gamedll.at(SHOOT), shoot as ShootFn)? };
    projectile::install(hooks, gamedll)?;
    log!("shooting from the barrel: a gun's shots leave its muzzle along its barrel");
    Ok(())
}

/// The player's aim direction, as the game shoots along it.
fn aim(player: usize) -> Option<[f32; 3]> {
    let getter = mem::read::<usize>(player).and_then(|vtable| mem::read::<usize>(vtable + AIM_SLOT))?;
    Module::find(crate::engine::GAMEDLL).filter(|m| m.contains(getter))?;
    let mut out = [0.0f32; 3];
    // SAFETY: the player's own aim getter, on the game thread, into our vec3.
    let result = unsafe { std::mem::transmute::<usize, AimFn>(getter)(player, &mut out) };
    let v = mem::read::<[f32; 3]>(result as usize)?;
    let l = hand_world::length(v);
    (l.is_finite() && l > 1e-6).then(|| v.map(|c| c / l))
}

fn barrel_shot(controller: usize, target: *const f32) -> Option<projectile::Shot> {
    // A shot at a point the game chose (a locked target) keeps its aim.
    let point = if target.is_null() { None } else { mem::read::<[f32; 3]>(target as usize) };
    if point.is_some_and(|p| p.iter().any(|c| *c != 0.0)) || !bow::players(controller) {
        return None;
    }
    let holder = hand_world::element(c"R_HandHolder")?;
    let z = [holder[2], holder[6], holder[10]];
    let l = hand_world::length(z);
    if !(l.is_finite() && l > 1e-6) {
        return None;
    }
    let barrel = z.map(|c| c / l);
    let start = [holder[3] + barrel[0] * MUZZLE, holder[7] + barrel[1] * MUZZLE, holder[11] + barrel[2] * MUZZLE];
    let aim = aim(hands::arms_model())?;
    Some(projectile::Shot::Turned { start, aim, barrel })
}

unsafe extern "C" fn shoot(controller: *mut c_void, target: *const f32) -> usize {
    let _flight = InFlight::enter();
    let shot = barrel_shot(controller as usize, target);
    if let Some(shot) = shot {
        let n = SHOTS.fetch_add(1, Relaxed) + 1;
        if n <= 20 {
            log!("shooting from the barrel: shot {n}: {shot:.2?}");
        }
    }
    // SAFETY: forwards the game's own call.
    projectile::aim(shot, || unsafe { SHOOT_ORIGINAL.get()(controller, target) })
}

pub fn report() {
    let n = SHOTS.load(Relaxed);
    if n > 0 {
        log!("shooting from the barrel: {n} shots, {} projectiles placed by the hands (arrows too)", projectile::placed());
    }
}
