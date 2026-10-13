//! Provider: the fire hook (`monaka_body::launch::Fires`), where the bow and gun ready-mades place
//! the player's shots.
//!
//! The projectiles the player's bow and guns fire, placed by the hands: every arrow and bullet is a
//! projectile from the level's pool, given a start and a velocity and fired by one function
//! ([`fire`]). While the player's bow looses (`crate::providers::bow`) or gun shoots ([`shoot`]),
//! what calls it says which weapon this shot is ([`aim`]), and each projectile fired asks that
//! weapon's ready-made where it goes (`monaka_body::launch::shot`, with the projectile as the game
//! fires it and the aim it made the shot along): its start and direction from the hands, its speed
//! kept unless the answer says its own.

use crate::Dl1;
use crate::providers::skeleton;
use monaka_body::launch::{self, Fired, Fires, Flight, Weapon};
use monaka_core::math::{length, unit};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use monaka_producer::game::{Lack, Start};
use std::ffi::c_void;
use std::sync::Mutex;

/// A bullet's start (all guns and bows; `Bullet`, taken from the level's pool): `(bullet)`. It keeps
/// the start and velocity it was given, attaches its trail and visual, sends it and runs its first
/// update.
const FIRE: usize = 0x7128c0;
/// In the projectile: its start, and its velocity.
const START: usize = 0x3c;
const VELOCITY: usize = 0x48;
/// `WeaponFireController`'s shot: `(controller, target point or a zero vector) -> fired`.
const SHOOT: usize = 0xda0160;
/// The player's look direction (its vfunc +0x748): `(player, out) -> out`. Throws aim along it too,
/// unless the game's eye tracking steers them.
const AIM_SLOT: usize = 0x748;

type FireFn = unsafe extern "C" fn(*mut c_void);
type ShootFn = unsafe extern "C" fn(*mut c_void, *const f32) -> usize;
type AimFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *const [f32; 3];

static FIRE_ORIGINAL: Original<FireFn> = Original::new();
static SHOOT_ORIGINAL: Original<ShootFn> = Original::new();

/// The shot being made: which weapon fires it, and the aim the game made it along where the
/// caller knows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Asked {
    pub weapon: Weapon,
    pub aim: Option<[f32; 3]>,
}

impl Asked {
    /// Where the projectile the game fires as `game` goes for this shot; none leaves it as fired (a
    /// projectile with no velocity, an answer of none).
    fn placed(&self, game: Flight) -> Option<Flight> {
        unit(game.velocity)?;
        let shot = launch::shot(&Fired { weapon: self.weapon, game, aim: self.aim, side: None })?;
        shot.flight(&game)
    }
}

static SHOT: Mutex<Option<Asked>> = Mutex::new(None);

/// The fire hook on the projectile's firing; the shots are scoped by the bow's loosing and the
/// gun's shot ([`aim`]).
impl Fires for Dl1 {
    const SPEED: bool = true;

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        Ok(install(start.hooks, &start.build.gamedll)?)
    }
}

/// Hooks the projectile's firing and the gun's shot (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    if FIRE_ORIGINAL.is_set() {
        return Ok(());
    }
    // SAFETY: the detours have the functions' signatures (read from their code and their callers);
    // the game DLL's build is checked by the caller; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&FIRE_ORIGINAL, "projectile fire", gamedll.at(FIRE), fire as FireFn)?;
        hooks.inline_decoded(&SHOOT_ORIGINAL, "gun shot", gamedll.at(SHOOT), shoot as ShootFn)?;
    }
    Ok(())
}

/// Whether the projectile hook is on (the throw probe leaves that function to it).
pub fn installed() -> bool {
    FIRE_ORIGINAL.is_set()
}

/// Runs `make` (the game's own loosing or shooting) with `shot` placing what it fires.
pub fn aim<R>(shot: Option<Asked>, make: impl FnOnce() -> R) -> R {
    if let Ok(mut current) = SHOT.lock() {
        *current = shot;
    }
    let result = make();
    if let Ok(mut current) = SHOT.lock() {
        *current = None;
    }
    result
}

unsafe extern "C" fn fire(projectile: *mut c_void) {
    let _flight = InFlight::enter();
    let at = projectile as usize;
    let shot = SHOT.lock().ok().and_then(|s| *s);
    if let Some(shot) = shot
        && let (Some(from), Some(velocity)) = (mem::read::<[f32; 3]>(at + START), mem::read::<[f32; 3]>(at + VELOCITY))
        && let Some(placed) = shot.placed(Flight { from, velocity })
    {
        for k in 0..3 {
            mem::write::<f32>(at + START + 4 * k, placed.from[k]);
            mem::write::<f32>(at + VELOCITY + 4 * k, placed.velocity[k]);
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { FIRE_ORIGINAL.get()(projectile) }
}

/// The player's aim direction, as the game shoots along it.
fn player_aim(player: usize) -> Option<[f32; 3]> {
    let getter = mem::read::<usize>(player).and_then(|vtable| mem::read::<usize>(vtable + AIM_SLOT))?;
    Module::find(crate::providers::engine::GAMEDLL).filter(|m| m.contains(getter))?;
    let mut out = [0.0f32; 3];
    // SAFETY: the player's own aim getter, on the game thread, into our vec3.
    let result = unsafe { std::mem::transmute::<usize, AimFn>(getter)(player, &mut out) };
    let v = mem::read::<[f32; 3]>(result as usize)?;
    let l = length(v);
    (l.is_finite() && l > 1e-6).then(|| v.map(|c| c / l))
}

/// The player's gun shot from `controller`, at `target` (a point the game chose, or a zero
/// vector): asked of the gun ready-made with the player's aim, unless the game aims at a point (a
/// locked target keeps its aim).
fn gun_shot(controller: usize, target: *const f32) -> Option<Asked> {
    let point = if target.is_null() { None } else { mem::read::<[f32; 3]>(target as usize) };
    if point.is_some_and(|p| p.iter().any(|c| *c != 0.0)) || !skeleton::players(controller) {
        return None;
    }
    Some(Asked { weapon: Weapon::Gun, aim: Some(player_aim(skeleton::arms_model())?) })
}

unsafe extern "C" fn shoot(controller: *mut c_void, target: *const f32) -> usize {
    let _flight = InFlight::enter();
    let shot = gun_shot(controller as usize, target);
    // SAFETY: forwards the game's own call.
    aim(shot, || unsafe { SHOOT_ORIGINAL.get()(controller, target) })
}

#[cfg(test)]
mod tests;
