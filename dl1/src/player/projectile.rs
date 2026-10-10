//! The projectiles the player's bow and guns fire, placed by the hands: every arrow and bullet is a
//! projectile from the level's pool, given a start and a velocity and fired by one function
//! ([`fire`]). While the player's bow looses or gun shoots, its module says what this shot is
//! ([`aim`]); each projectile fired then gets its start and direction from the hands, its speed
//! kept.

use crate::player::hand_world;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// A projectile's firing (all guns and bows): `(projectile)`.
const FIRE: usize = 0x7128c0;
/// In the projectile: its start, and its velocity.
const START: usize = 0x3c;
const VELOCITY: usize = 0x48;

type FireFn = unsafe extern "C" fn(*mut c_void);

static FIRE_ORIGINAL: Original<FireFn> = Original::new();

/// What the shot being made is.
#[derive(Clone, Copy, Debug)]
pub enum Shot {
    /// From `start` along `direction` (unit).
    Along { start: [f32; 3], direction: [f32; 3] },
    /// From `start`, each projectile turned from `aim` (the game's aim, unit) onto `barrel` (unit),
    /// so a pellet's spread around the aim becomes the same spread around the barrel.
    Turned { start: [f32; 3], aim: [f32; 3], barrel: [f32; 3] },
}

static SHOT: Mutex<Option<Shot>> = Mutex::new(None);
static PLACED: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    if FIRE_ORIGINAL.is_set() {
        return Ok(());
    }
    // SAFETY: the detour has the function's signature (read from its code); the game DLL's build
    // is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&FIRE_ORIGINAL, "projectile fire", gamedll.at(FIRE), fire as FireFn)? };
    Ok(())
}

/// Whether the projectile hook is on (the throw probe leaves that function to it).
pub fn installed() -> bool {
    FIRE_ORIGINAL.is_set()
}

/// Runs `make` (the game's own loosing or shooting) with `shot` placing what it fires.
pub fn aim<R>(shot: Option<Shot>, make: impl FnOnce() -> R) -> R {
    if let Ok(mut current) = SHOT.lock() {
        *current = shot;
    }
    let result = make();
    if let Ok(mut current) = SHOT.lock() {
        *current = None;
    }
    result
}

pub fn placed() -> u64 {
    PLACED.load(Relaxed)
}

fn unit(v: [f32; 3]) -> Option<[f32; 3]> {
    let l = hand_world::length(v);
    (l.is_finite() && l > 1e-6).then(|| v.map(|c| c / l))
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// `v` turned by the rotation that takes unit `from` to unit `to`.
pub fn turned(v: [f32; 3], from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    let axis = cross(from, to);
    let (sin, cos) = (hand_world::length(axis), dot(from, to));
    let Some(k) = unit(axis).filter(|_| sin > 1e-6) else { return v };
    let kv = cross(k, v);
    let kd = dot(k, v) * (1.0 - cos);
    [0, 1, 2].map(|i| v[i] * cos + kv[i] * sin + k[i] * kd)
}

unsafe extern "C" fn fire(projectile: *mut c_void) {
    let _flight = InFlight::enter();
    let at = projectile as usize;
    let shot = SHOT.lock().ok().and_then(|s| *s);
    if let Some(shot) = shot
        && let Some(velocity) = mem::read::<[f32; 3]>(at + VELOCITY)
        && let Some(along) = unit(velocity)
    {
        let speed = hand_world::length(velocity);
        let (start, direction) = match shot {
            Shot::Along { start, direction } => (start, direction),
            // This pellet's turn away from the aim, put on the barrel.
            Shot::Turned { start, aim, barrel } => (start, turned(barrel, aim, along)),
        };
        for k in 0..3 {
            mem::write::<f32>(at + START + 4 * k, start[k]);
            mem::write::<f32>(at + VELOCITY + 4 * k, direction[k] * speed);
        }
        PLACED.fetch_add(1, Relaxed);
    }
    // SAFETY: forwards the game's own call.
    unsafe { FIRE_ORIGINAL.get()(projectile) }
}
