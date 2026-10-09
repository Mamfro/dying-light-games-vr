//! The player's walk body: the engine's
//! `CODEPhysicsWalkFly`, stepped at 50 Hz by its vtable slot 9 `(body, dt)`. Hooked for room-scale
//! following ([`crate::player::roomscale`]) and its probe ([`crate::research::walk`]).
//!
//! Extra velocity goes into the body's wanted velocity, the one the stick sets: the game writes it
//! only when it changes it and reads it outside the step, so the extra stays in it, on top of the
//! game's own value (measured 2026-10-08: the body then moves at exactly that velocity and stops
//! against walls). `AdditionalLinVel` did nothing to a standing body.

use crate::engine;
use crate::player::hands;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::*};

/// `CODEPhysicsWalkFly`'s step (its primary vtable's slot 9): `(full object, dt in xmm1)`.
const STEP: usize = 0x3d6f50;
const PHYSICS_BIND: &str = "?PhysicsBind@IControlObject@@QEAAPEAVIPhysicsBind@@XZ";
const GET_WORLD_POSITION: &str = "?GetWorldPosition@IControlObject@@QEBA?AVvec3@@XZ";
/// The player's control object, from the player (`PlayerDI`).
const PLAYER_CONTROL: usize = 0x18;
/// The physics bind's walk getter (`IPhysicsWalk*`, the full object's +0x700).
const BIND_WALK: usize = 0xb0;
/// The `IPhysicsWalk` interface in the full object.
pub const WALK: usize = 0x700;
/// The walk interface's body getter (`IPhBody*`): the body's position is at +0xbc (0.94 m above
/// the character's), its velocity at +0xd8.
const GET_BODY: usize = 0x2c0;
pub const BODY_POSITION: usize = 0xbc;
pub const BODY_VELOCITY: usize = 0xd8;
pub const WANTED_VELOCITY: usize = 0xa70;
pub const EXTRA_VELOCITY: usize = 0xa94;
/// The step moves the body toward a target of its own ("move to"), not the wanted velocity.
pub const SCRIPTED_MOVE: usize = 0x7e8;
pub const LOCK_POSITION: usize = 0x850;
pub const STATE: usize = 0x9c0;
pub const PHYSICS_OFF: usize = 0x2b1;

type StepFn = unsafe extern "C" fn(*mut c_void, f32);
type PhysicsBindFn = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
type GetterFn = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
/// `vec3 IControlObject::GetWorldPosition() const`: the result through a hidden pointer.
type WorldPositionFn = unsafe extern "system" fn(*mut c_void, *mut [f32; 3]) -> *mut [f32; 3];

static STEP_ORIGINAL: Original<StepFn> = Original::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);
static PHYSICS_BIND_FN: AtomicUsize = AtomicUsize::new(0);
static WORLD_POSITION_FN: AtomicUsize = AtomicUsize::new(0);
/// The player whose walk interface is cached, and that interface.
static PLAYER: AtomicUsize = AtomicUsize::new(0);
static PLAYER_WALK: AtomicUsize = AtomicUsize::new(0);

pub fn install(hooks: &mut Hooks, engine: &Module) -> Result<(), Rejection> {
    if INSTALLED.swap(true, Relaxed) {
        return Ok(());
    }
    let bind = engine.export(PHYSICS_BIND).ok_or_else(|| Rejection::revision("the engine does not export IControlObject::PhysicsBind"))?;
    PHYSICS_BIND_FN.store(bind, Relaxed);
    WORLD_POSITION_FN.store(engine.export(GET_WORLD_POSITION).unwrap_or(0), Relaxed);
    // SAFETY: the detour has the step's signature (read from its code: the body, then dt in
    // xmm1); the engine's build is checked at start; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&STEP_ORIGINAL, "walk body step", engine.at(STEP), step as StepFn)? };
    Ok(())
}

/// The player's walk interface, re-read when the player changes.
fn player_walk() -> usize {
    let player = hands::arms_model();
    if player == 0 {
        return 0;
    }
    if PLAYER.load(Relaxed) == player {
        return PLAYER_WALK.load(Relaxed);
    }
    let bind_fn = PHYSICS_BIND_FN.load(Relaxed);
    if bind_fn == 0 {
        return 0;
    }
    // SAFETY: the engine's export on the player's control object (`PlayerDI`+0x18), as the game
    // calls it; it reads two pointers.
    let bind = unsafe { std::mem::transmute::<usize, PhysicsBindFn>(bind_fn)((player + PLAYER_CONTROL) as *mut c_void) } as usize;
    let walk = mem::read::<usize>(bind)
        .and_then(|vtable| mem::read::<usize>(vtable + BIND_WALK))
        .filter(|&get| Module::find(engine::ENGINE).is_some_and(|m| m.contains(get)))
        // SAFETY: the bind's own getter (`GetPhysics` filtered to the walk type).
        .map_or(0, |get| unsafe { std::mem::transmute::<usize, GetterFn>(get)(bind as *mut c_void) } as usize);
    PLAYER.store(player, Relaxed);
    PLAYER_WALK.store(walk, Relaxed);
    log!("walk: player {player:#x} has the walk interface {walk:#x} (bind {bind:#x})");
    walk
}

pub fn vec3(at: usize) -> [f32; 3] {
    mem::read::<[f32; 3]>(at).unwrap_or([f32::NAN; 3])
}

/// The walk body's physics body (`IPhBody`), 0 if unreadable.
pub fn body(full: usize) -> usize {
    let walk = full + WALK;
    mem::read::<usize>(walk)
        .and_then(|vtable| mem::read::<usize>(vtable + GET_BODY))
        .filter(|&get| Module::find(engine::ENGINE).is_some_and(|m| m.contains(get)))
        // SAFETY: the walk interface's own body getter.
        .map_or(0, |get| unsafe { std::mem::transmute::<usize, GetterFn>(get)(walk as *mut c_void) } as usize)
}

/// The player's position as the engine reports it (`IControlObject::GetWorldPosition`).
pub fn player_position() -> [f32; 3] {
    let control = PLAYER.load(Relaxed) + PLAYER_CONTROL;
    match WORLD_POSITION_FN.load(Relaxed) {
        0 => [f32::NAN; 3],
        _ if control == PLAYER_CONTROL => [f32::NAN; 3],
        f => {
            let mut out = [0.0f32; 3];
            // SAFETY: the engine's export on the player's control object, into our vec3.
            unsafe { std::mem::transmute::<usize, WorldPositionFn>(f)(control as *mut c_void, &mut out) };
            out
        }
    }
}

/// The game's wanted velocity as it last set it, and what was last written over it.
static WANTED: Mutex<([f32; 3], [f32; 3])> = Mutex::new(([0.0; 3], [f32::NAN; 3]));

/// The wanted velocity becomes the game's own plus `extra` (horizontal, world x and z); returns
/// the game's own. A value other than the one last written is the game's new one.
pub fn set_extra(full: usize, extra: [f32; 2]) -> [f32; 3] {
    let at = full + WANTED_VELOCITY;
    let now = vec3(at);
    let Ok(mut wanted) = WANTED.lock() else { return now };
    if now != wanted.1 {
        wanted.0 = now;
    }
    let game = wanted.0;
    let written = [game[0] + extra[0], game[1], game[2] + extra[1]];
    if written.iter().all(|c| c.is_finite()) && mem::write(at, written) {
        wanted.1 = written;
    }
    game
}

/// Gives the wanted velocity back to the game at the end of a run (left with an extra in it, the
/// character would walk on), unless the game has set a new one since.
pub fn restore() {
    let walk = PLAYER_WALK.load(Relaxed);
    if walk != 0
        && let Ok(wanted) = WANTED.lock()
        && wanted.1.iter().all(|c| c.is_finite())
    {
        let at = walk - WALK + WANTED_VELOCITY;
        if vec3(at) == wanted.1 {
            mem::write(at, wanted.0);
            log!("walk: the wanted velocity is the game's {:.2?} again", wanted.0);
        }
    }
}

unsafe extern "C" fn step(full: *mut c_void, dt: f32) {
    let _flight = InFlight::enter();
    let at = full as usize;
    let walk = player_walk();
    let player = walk != 0 && at + WALK == walk;
    if player {
        let mut extra = crate::player::roomscale::before_step(at, dt);
        if let Some(push) = crate::research::walk::push() {
            extra = [extra[0] + push[0], extra[1] + push[1]];
        }
        crate::player::roomscale::asked(set_extra(at, extra));
    }
    // SAFETY: forwards the engine's own call.
    unsafe { STEP_ORIGINAL.get()(full, dt) };
    if player {
        crate::research::walk::note(at, dt);
    }
}
