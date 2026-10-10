//! `probe_throw`: what a throw makes, for throwing by hand. Logs each throw of a throwable (the
//! throw controller's throw: its state and the item's kind) and each drop or throw of a held item
//! by the player (a melee weapon thrown at a target point), and for every world object made from
//! an item while one runs: its class, where it is, and the vector at +0x100 (the throwable's
//! velocity) right after, and only then: an object can be gone by the next frame, and reading a
//! gone one crashes the engine. Also each arrow the bow looses: how far the bow was drawn, and
//! the projectile's start and velocity as it is fired. And each launch of a throwable object
//! (molotovs, grenades): the velocity it is given and the one it has after.

use monaka_hook::module::Module;
use monaka_hook::probe::{callers, class_name};
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// `WeaponThrowController`'s throw: `(controller)`.
const THROW: usize = 0xdd1ad0;
/// `PlayerDI`'s drop or throw of an item (its vtable slot +0xa28): `(player, slot, item, flag,
/// target point or null vector, ...)`, returning the world object.
const DROP_OR_THROW: usize = 0xb9eef0;
/// Makes the world object for an inventory item: `(item, player, mode)`.
const SPAWN: usize = 0x6fa610;
/// In the throw controller: its state, and whether it holds an item (then the item at +0x98).
const STATE: usize = 0x38;
const HOLDS: usize = 0x90;
const HELD: usize = 0x98;
/// The item's object (at +0x58) and its kind (vfunc +0x1c0).
const ITEM_OBJECT: usize = 0x58;
const KIND_SLOT: usize = 0x1c0;
/// In a world object: its control object (+0x18), and the vector the throw writes (+0x100).
const CONTROL: usize = 0x18;
const VELOCITY: usize = 0x100;
/// `WeaponBowController`'s loosing of an arrow: `(controller)`; the draw (0 to 1) at +0x6c.
const BOW_FIRE: usize = 0xd9cf40;
const BOW_DRAW: usize = 0x6c;
/// A projectile's firing: `(projectile)`; its start at +0x3c, its velocity at +0x48.
const PROJECTILE_FIRE: usize = 0x7128c0;
const PROJECTILE_START: usize = 0x3c;
const PROJECTILE_VELOCITY: usize = 0x48;
/// `ThrowableObject`'s launch: `(object, velocity or a zero vector, target point or null)`.
const LAUNCH: usize = 0xeea9f0;
const GET_WORLD_POSITION: &str = "?GetWorldPosition@IControlObject@@QEBA?AVvec3@@XZ";

type ThrowFn = unsafe extern "C" fn(*mut c_void);
type DropFn = unsafe extern "C" fn(*mut c_void, u32, usize, u8, *const f32, usize, usize, usize, usize) -> usize;
type SpawnFn = unsafe extern "C" fn(usize, usize, u32) -> usize;
type KindFn = unsafe extern "C" fn(usize) -> u32;
type PositionFn = unsafe extern "system" fn(usize, *mut [f32; 3]) -> *mut [f32; 3];

static THROW_ORIGINAL: Original<ThrowFn> = Original::new();
static DROP_ORIGINAL: Original<DropFn> = Original::new();
static SPAWN_ORIGINAL: Original<SpawnFn> = Original::new();
static POSITION: AtomicUsize = AtomicUsize::new(0);
/// While a throw or drop runs (its name), the objects made.
static RUNNING: Mutex<Option<(&'static str, Vec<usize>)>> = Mutex::new(None);
static THROWS: AtomicU64 = AtomicU64::new(0);
static BOW_FIRE_ORIGINAL: Original<ThrowFn> = Original::new();
static PROJECTILE_FIRE_ORIGINAL: Original<ThrowFn> = Original::new();
/// The bow's draw while it looses an arrow.
static LOOSING: Mutex<Option<f32>> = Mutex::new(None);
static ARROWS: AtomicU64 = AtomicU64::new(0);
type LaunchFn = unsafe extern "C" fn(usize, *const [f32; 3], *const [f32; 3]);
static LAUNCH_ORIGINAL: Original<LaunchFn> = Original::new();
static LAUNCHES: AtomicU64 = AtomicU64::new(0);

fn vec3(at: usize) -> [f32; 3] {
    mem::read::<[f32; 3]>(at).unwrap_or([f32::NAN; 3])
}

fn position(object: usize) -> [f32; 3] {
    let f = POSITION.load(Relaxed);
    if f == 0 || object == 0 {
        return [f32::NAN; 3];
    }
    let mut out = [0.0f32; 3];
    // SAFETY: the engine's export on the object's control object, into our vec3.
    unsafe { std::mem::transmute::<usize, PositionFn>(f)(object + CONTROL, &mut out) };
    out
}

fn describe(object: usize) -> String {
    format!("{object:#x} ({}) at {:.2?}, +0x100 {:.2?}", class_name(object).unwrap_or_default(), position(object), vec3(object + VELOCITY))
}

fn begin(name: &'static str) {
    if let Ok(mut running) = RUNNING.lock() {
        *running = Some((name, Vec::new()));
    }
}

fn end() -> Vec<usize> {
    RUNNING.lock().ok().and_then(|mut r| r.take()).map(|(_, made)| made).unwrap_or_default()
}

unsafe extern "C" fn throw(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let at = controller as usize;
    let item = if mem::read::<usize>(at + HOLDS).unwrap_or(0) != 0 { mem::read::<usize>(at + HELD).unwrap_or(0) } else { 0 };
    let kind = (item != 0)
        .then(|| mem::read::<usize>(item + ITEM_OBJECT).map(|_| item + ITEM_OBJECT))
        .flatten()
        .and_then(|object| mem::read::<usize>(object).and_then(|vtable| mem::read::<usize>(vtable + KIND_SLOT)).map(|f| (object, f)))
        // SAFETY: the item object's own kind getter.
        .map(|(object, f)| unsafe { std::mem::transmute::<usize, KindFn>(f)(object) });
    let n = THROWS.fetch_add(1, Relaxed) + 1;
    log!("probe_throw: throw {n}: state {:?}, item {item:#x}, kind {kind:?}", mem::read::<u32>(at + STATE));
    begin("throw");
    // SAFETY: forwards the game's own call.
    unsafe { THROW_ORIGINAL.get()(controller) };
    for object in end() {
        log!("probe_throw: throw {n} made {}", describe(object));
    }
}

unsafe extern "C" fn drop_or_throw(player: *mut c_void, slot: u32, item: usize, flag: u8, target: *const f32, a: usize, b: usize, c: usize, d: usize) -> usize {
    let _flight = InFlight::enter();
    let point = if target.is_null() { [f32::NAN; 3] } else { vec3(target as usize) };
    begin("drop");
    // SAFETY: forwards the game's own call.
    let object = unsafe { DROP_ORIGINAL.get()(player, slot, item, flag, target, a, b, c, d) };
    let made = end();
    let chain: Vec<String> = callers(6).into_iter().map(Module::describe).collect();
    log!("probe_throw: drop or throw: slot {slot}, item {item:#x}, flag {flag}, target {point:.2?} -> {}; made {}; from {}", describe(object), made.len(), chain.join(" < "));
    object
}

unsafe extern "C" fn bow_fire(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let draw = mem::read::<f32>(controller as usize + BOW_DRAW).unwrap_or(f32::NAN);
    if let Ok(mut loosing) = LOOSING.lock() {
        *loosing = Some(draw);
    }
    // SAFETY: forwards the game's own call.
    unsafe { BOW_FIRE_ORIGINAL.get()(controller) };
    if let Ok(mut loosing) = LOOSING.lock() {
        *loosing = None;
    }
}

unsafe extern "C" fn projectile_fire(projectile: *mut c_void) {
    let _flight = InFlight::enter();
    let at = projectile as usize;
    if let Some(draw) = LOOSING.lock().ok().and_then(|l| *l) {
        let velocity = vec3(at + PROJECTILE_VELOCITY);
        let speed = (velocity[0] * velocity[0] + velocity[1] * velocity[1] + velocity[2] * velocity[2]).sqrt();
        let n = ARROWS.fetch_add(1, Relaxed) + 1;
        log!("probe_throw: arrow {n}: draw {draw:.2}, start {:.2?}, velocity {velocity:.2?} ({speed:.1} m/s)", vec3(at + PROJECTILE_START));
    }
    // SAFETY: forwards the game's own call.
    unsafe { PROJECTILE_FIRE_ORIGINAL.get()(projectile) }
}

unsafe extern "C" fn launch(object: usize, velocity: *const [f32; 3], target: *const [f32; 3]) {
    let _flight = InFlight::enter();
    let given = mem::read::<[f32; 3]>(velocity as usize);
    let point = (!target.is_null()).then(|| mem::read::<[f32; 3]>(target as usize)).flatten();
    // SAFETY: forwards the game's own call.
    unsafe { LAUNCH_ORIGINAL.get()(object, velocity, target) };
    let after = vec3(object + VELOCITY);
    let speed = (after[0] * after[0] + after[1] * after[1] + after[2] * after[2]).sqrt();
    let n = LAUNCHES.fetch_add(1, Relaxed) + 1;
    log!("probe_throw: launch {n}: {} given {given:.2?}, target {point:.2?}; after {after:.2?} ({speed:.1} m/s)", describe(object));
}

unsafe extern "C" fn spawn(item: usize, player: usize, mode: u32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let object = unsafe { SPAWN_ORIGINAL.get()(item, player, mode) };
    if let Ok(mut running) = RUNNING.lock()
        && let Some((name, made)) = running.as_mut()
    {
        log!("probe_throw: {name} spawns (mode {mode}) {}", describe(object));
        made.push(object);
    }
    object
}

pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module) -> Result<(), Rejection> {
    POSITION.store(engine_module.export(GET_WORLD_POSITION).unwrap_or(0), Relaxed);
    // SAFETY: each detour has its function's signature (read from its code); the game DLL's build
    // is checked at start; the prologues are decoded and moved.
    // Functions the throwing and bow features hook already are left to them.
    unsafe {
        hooks.inline_decoded(&DROP_ORIGINAL, "player drop or throw", gamedll.at(DROP_OR_THROW), drop_or_throw as DropFn)?;
        if crate::player::throwing::installed() {
            log!("probe_throw: throwing by hand is on: its throw and spawn hooks are not doubled");
        } else {
            hooks.inline_decoded(&THROW_ORIGINAL, "throw controller throw", gamedll.at(THROW), throw as ThrowFn)?;
            hooks.inline_decoded(&SPAWN_ORIGINAL, "item object spawn", gamedll.at(SPAWN), spawn as SpawnFn)?;
            hooks.inline_decoded(&LAUNCH_ORIGINAL, "throwable launch", gamedll.at(LAUNCH), launch as LaunchFn)?;
        }
        if crate::player::projectile::installed() {
            log!("probe_throw: the bow by hand or shooting from the barrel is on: the loose and fire hooks are not doubled");
        } else {
            hooks.inline_decoded(&BOW_FIRE_ORIGINAL, "bow loose", gamedll.at(BOW_FIRE), bow_fire as ThrowFn)?;
            hooks.inline_decoded(&PROJECTILE_FIRE_ORIGINAL, "projectile fire", gamedll.at(PROJECTILE_FIRE), projectile_fire as ThrowFn)?;
        }
    }
    log!("probe_throw: watching throws, drops and the objects they make");
    Ok(())
}

pub fn report() {
    log!("probe_throw: {} throws, {} launches, {} arrows", THROWS.load(Relaxed), LAUNCHES.load(Relaxed), ARROWS.load(Relaxed));
}
