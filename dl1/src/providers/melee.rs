//! Provider: melee hits (`monaka_body::melee_hit::Hits`): Dying Light 1's hit detection, what its
//! melee can hit, and where it types a hit. Its component is `crate::MELEE`.
//!
//! DL1's own hit detection sweeps a blade along the arms' `R_HandHolder` element frame to frame and
//! tests it against each enemy's bones, and the hand rig poses that element; three things in it are
//! made for the canned attack animation instead, and are changed for the player's controller:
//!
//! - it sweeps only in a window the attack sets up at its start (about 290 ms after the press, for
//!   about 260 ms), by when a real swing is over: the window here opens as the controller enters
//!   an attack state ([`hit_dir_set`]);
//! - the hit's direction is the attack animation's (a fixed sideways direction per attack type),
//!   not the blade's motion: that override is cleared, so the game keeps the direction the hand
//!   moved;
//! - the blade reaches 1.3 times the weapon's range plus 0.1 m (2.4 m for a machete): here it is
//!   the length of the weapon in the hand (`monaka_body::melee_hit::blade`), or a fist's reach
//!   ([`reach`]).
//!
//! What the melee can hit is found by the level's search with the class the game's melee passes,
//! the player and the AI's target markers left out ([`Hits::near`]); the look runs at each update
//! of the player's melee controller ([`update`]). Every hit passes `IControlObject::TakeDamage`
//! ([`take_damage`]): the player's cut not led by the edge lands blunt.

use crate::Dl1;
use crate::providers::{engine, skeleton};
use monaka_body::held::Kind;
use monaka_body::melee_hit::{self, Hits};
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// The melee controller setting its hit direction (`state`): as the controller changes state (a new
/// attack, a charge let go, a stop, ...), the attack's direction and its hit window for that state.
const SET_HIT_DIR: usize = 0xdc9320;
/// The controller states that are attacks, which it sets up (bit per state, from its own check).
const ATTACK_STATES: u32 = 0x62ffff;
/// The blade's reach for this frame, the melee controller's damage range for the look angle:
/// `(controller, weapon) -> metres`, the weapon's range scaled for how far the player looks up or
/// down.
const REACH: usize = 0xdbec70;
/// The attack's direction (player-local); non-zero overrides each hit's own direction.
const ATTACK_DIRECTION: usize = 0x29c;
/// When the attack starts looking for hits, seconds after it starts.
const WINDOW_START: usize = 0x2a8;
/// How far past the reach the hit test extends the blade: `reach * 1.3 + 0.1`.
const REACH_SCALE: f32 = 1.3;
const REACH_EXTRA: f32 = 0.1;

/// The melee controller's update (`WeaponMeleeController::Update`), every game update:
/// `(controller)` (its time step is unused).
const UPDATE: usize = 0xdb1d10;
/// The class the game's melee looks for targets of (its broadphase's `FindObjectsInRadius` filter):
/// `ObjectDI`'s RTTI.
const TARGETS: usize = 0x1cb11d0;
/// The object a found `IControlObject` belongs to: 0x18 before it.
const CONTROL_OBJECT_IN_OBJECT: usize = 0x18;
const ACTIVE_LEVEL: &str = "?GetActiveLevel@IGame@@QEAAPEAVILevel@@XZ";
const FIND_IN_RADIUS: &str = "?FindObjectsInRadius@ILevel@@QEAA_NPEAV?$vector@PEAVIControlObject@@@ttl@@AEBVvec3@@MPEBVCRTTI@@_NPEAH@Z";
const EXTENTS: &str = "?GetExtentsInWorld@IControlObject@@QEBAAEBVextents@@XZ";
/// `IControlObject::TakeDamage(SDamageInfo const&)`: every hit passes it before the victim reacts.
const TAKE_DAMAGE: &str = "?TakeDamage@IControlObject@@UEAAXAEBUSDamageInfo@@@Z";
/// In the damage info: the attacker (for the player's melee hits, the player object plus 0x28) and
/// the damage type (`data/enums/damage_type.def`).
const INFO_ATTACKER: usize = 0x08;
const ATTACKER_IN_PLAYER: usize = 0x28;
const INFO_DAMAGE_TYPE: usize = 0x3c;
const DAMAGE_CUT: i32 = 1;
const DAMAGE_BLUNT: i32 = 18;

type SetHitDirFn = unsafe extern "C" fn(*mut c_void, i32);
type TakeDamageFn = unsafe extern "C" fn(*mut c_void, *mut u8);
type UpdateFn = unsafe extern "C" fn(*mut c_void);
type ReachFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> f32;
type ActiveLevelFn = unsafe extern "system" fn(usize) -> usize;
type FindInRadiusFn = unsafe extern "system" fn(usize, *mut Found, *const [f32; 3], f32, usize, bool, *mut i32) -> bool;
type ExtentsFn = unsafe extern "system" fn(usize) -> *const [f32; 6];

/// The engine's `ttl::vector<IControlObject*>` the search fills (grown with the engine's own
/// allocator; kept, never freed, so it is only ever grown by the engine).
#[repr(C)]
struct Found {
    items: *mut usize,
    count: u32,
    capacity: u32,
}

// SAFETY: only used on the game's thread, under its mutex.
unsafe impl Send for Found {}

struct Search {
    /// The engine's base, where the game object's global is read ([`engine::game_at`]).
    engine_base: usize,
    active_level: ActiveLevelFn,
    find: FindInRadiusFn,
    extents: ExtentsFn,
    targets: usize,
}

static SEARCH: OnceLock<Search> = OnceLock::new();
static FOUND: Mutex<Found> = Mutex::new(Found { items: std::ptr::null_mut(), count: 0, capacity: 0 });
static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static TAKE_DAMAGE_ORIGINAL: Original<TakeDamageFn> = Original::new();
static SET_HIT_DIR_ORIGINAL: Original<SetHitDirFn> = Original::new();
static REACH_ORIGINAL: Original<ReachFn> = Original::new();
/// Object classes (by vtable) the search leaves out: the game's invisible AI target markers, some
/// of them with a body-sized box.
static MARKERS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
static BLUNTED: AtomicU64 = AtomicU64::new(0);
static EDGED: AtomicU64 = AtomicU64::new(0);
static DAMAGE_SEEN: AtomicU64 = AtomicU64::new(0);
static ATTACKS: AtomicU64 = AtomicU64::new(0);
static REACHES: AtomicU64 = AtomicU64::new(0);

/// The hit window, the hit's direction and the blade's reach changed for the player's controller;
/// the look at each update of the player's melee controller; each hit typed at `TakeDamage`.
impl Hits for Dl1 {
    const ATTACKS_ON_RELEASE: bool = true;

    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        let gamedll = &start.build.gamedll;
        // SAFETY: the detours have the targets' signatures (read from their code and call sites);
        // the game DLL's build is checked by the caller; prologues are decoded and moved.
        unsafe {
            start.hooks.inline_decoded(&SET_HIT_DIR_ORIGINAL, "melee hit direction", gamedll.at(SET_HIT_DIR), hit_dir_set as SetHitDirFn)?;
            start.hooks.inline_decoded(&REACH_ORIGINAL, "melee reach", gamedll.at(REACH), reach as ReachFn)?;
        }
        Ok(())
    }

    fn install_look(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        Ok(install_look(start.hooks, &start.build.gamedll, &start.build.engine)?)
    }

    fn install_blunt(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        let target = start.build.engine.export(TAKE_DAMAGE).ok_or_else(|| Rejection::revision("the engine does not export IControlObject::TakeDamage"))?;
        // SAFETY: the export takes the victim and its damage info (its mangled name); the prologue
        // is decoded and moved.
        unsafe { start.hooks.inline_decoded(&TAKE_DAMAGE_ORIGINAL, "IControlObject::TakeDamage", target, take_damage as TakeDamageFn)? };
        Ok(())
    }

    fn near(centre: [f32; 3], radius: f32) -> Option<Vec<(usize, [f32; 6])>> {
        near(centre, radius)
    }

    fn name(id: usize) -> String {
        monaka_hook::probe::class_name(id).unwrap_or_else(|| "?".into())
    }
}

fn install_look(hooks: &mut monaka_hook::Hooks, gamedll: &Module, engine_module: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let resolved = unsafe {
        Search {
            engine_base: engine_module.at(0),
            active_level: std::mem::transmute::<usize, ActiveLevelFn>(find(ACTIVE_LEVEL)?),
            find: std::mem::transmute::<usize, FindInRadiusFn>(find(FIND_IN_RADIUS)?),
            extents: std::mem::transmute::<usize, ExtentsFn>(find(EXTENTS)?),
            targets: gamedll.at(TARGETS),
        }
    };
    let _ = SEARCH.set(resolved);
    // SAFETY: the controller's update takes the controller (read from its code); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "melee controller update", gamedll.at(UPDATE), update as UpdateFn)? };
    Ok(())
}

unsafe extern "C" fn update(controller: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller) };
    if skeleton::players(controller as usize) {
        melee_hit::look::<Dl1>();
    }
}

/// What the game's melee can hit within `radius` of `centre`: each control object the level's
/// search finds with the melee's class, its box, the player and the AI's target markers left out.
fn near(centre: [f32; 3], radius: f32) -> Option<Vec<(usize, [f32; 6])>> {
    let search = SEARCH.get()?;
    let player = skeleton::arms_model();
    let game = engine::game_at(search.engine_base).filter(|_| player != 0)?;
    // SAFETY: the engine's own call on the live game object, on the game's thread.
    let level = unsafe { (search.active_level)(game) };
    if level == 0 {
        return None;
    }
    let mut found = FOUND.locked();
    found.count = 0;
    // SAFETY: the engine's search on its live level; `found` is a vector of its kind, which it
    // grows with its own allocator; the filter is the class the game's melee passes.
    unsafe { (search.find)(level, &mut *found, &centre, radius, search.targets, false, std::ptr::null_mut()) };
    let mut near = Vec::new();
    for i in 0..found.count as usize {
        let Some(object) = mem::read::<usize>(found.items as usize + i * 8).filter(|&o| o != 0) else { continue };
        let owner = object - CONTROL_OBJECT_IN_OBJECT;
        if owner == player || object == player || is_marker(owner) {
            continue;
        }
        // SAFETY: the engine's getter on a live control object.
        if let Some(b) = mem::read::<[f32; 6]>(unsafe { (search.extents)(object) } as usize) {
            near.push((owner, b));
        }
    }
    Some(near)
}

/// Whether `object` is one of the game's AI target markers (`NightFakeAITarget`,
/// `SoudFakeAITarget`, `FakeAITarget`), by its class, looked up once per vtable.
fn is_marker(object: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(object) else { return false };
    let mut known = MARKERS.locked();
    if let Some(&(_, marker)) = known.iter().find(|(v, _)| *v == vtable) {
        return marker;
    }
    let marker = monaka_hook::probe::class_name(object).is_some_and(|c| c.contains("FakeAITarget"));
    if known.len() < 256 {
        known.push((vtable, marker));
    }
    marker
}

/// Every hit: the player's cut that did not lead with the edge lands blunt.
unsafe extern "C" fn take_damage(victim: *mut c_void, info: *mut u8) {
    let _flight = InFlight::enter();
    let at = info as usize;
    let model = skeleton::arms_model();
    let ours = model != 0 && !info.is_null() && mem::read::<usize>(at + INFO_ATTACKER) == Some(model + ATTACKER_IN_PLAYER);
    // Diagnostic: the first hits on characters, with what the rule reads.
    if !info.is_null() && monaka_hook::probe::class_name(victim as usize).is_some_and(|c| c.contains("HumanAI")) && DAMAGE_SEEN.fetch_add(1, Relaxed) < 30 {
        log!(
            "physical melee: damage to {victim:p}: attacker {:#x} (the player's would be {:#x}), type {}, holding {:?}, led by the edge {:?}",
            mem::read::<usize>(at + INFO_ATTACKER).unwrap_or(0),
            model + ATTACKER_IN_PLAYER,
            mem::read::<i32>(at + INFO_DAMAGE_TYPE).unwrap_or(-1),
            melee_hit::wielded(),
            melee_hit::edge_led(),
        );
    }
    if ours && mem::read::<i32>(at + INFO_DAMAGE_TYPE) == Some(DAMAGE_CUT) && melee_hit::wielded().is_some_and(|h| matches!(h, Kind::Melee | Kind::Blade)) {
        match melee_hit::edge_led() {
            Some(false) => {
                // The game's own damage info for this hit, on its thread, before the victim reads it.
                mem::write::<i32>(at + INFO_DAMAGE_TYPE, DAMAGE_BLUNT);
                let n = BLUNTED.fetch_add(1, Relaxed) + 1;
                if n <= 40 {
                    log!("physical melee: hit {n} not led by the edge: cut -> blunt");
                }
            }
            led => {
                let n = EDGED.fetch_add(1, Relaxed) + 1;
                if n <= 40 {
                    log!("physical melee: edge hit {n} (led by the edge {led:?}): a cut");
                }
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { TAKE_DAMAGE_ORIGINAL.get()(victim, info) }
}

/// The player's melee controller, with what the arms hold: the controller's player is the arms
/// model the rig poses, and the arms hold no gun.
fn player_melee(controller: *mut c_void) -> Option<Kind> {
    let held = melee_hit::wielded().filter(|&h| h != Kind::Gun)?;
    skeleton::players(controller as usize).then_some(held)
}

unsafe extern "C" fn hit_dir_set(controller: *mut c_void, state: i32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { SET_HIT_DIR_ORIGINAL.get()(controller, state) };
    // The game returns at once for anything but the attack states.
    if !(0..=0x16).contains(&state) || (ATTACK_STATES >> state) & 1 == 0 {
        return;
    }
    let Some(held) = player_melee(controller) else { return };
    let at = controller as usize;
    let window = mem::read::<f32>(at + WINDOW_START).unwrap_or(f32::NAN);
    // No window set up: nothing to open (as in DL2 and The Beast).
    if window.is_nan() || window < 0.0 {
        return;
    }
    let direction = mem::read::<[f32; 3]>(at + ATTACK_DIRECTION).unwrap_or([f32::NAN; 3]);
    // The game's own fields of its own controller, written on the game thread inside its call.
    mem::write::<f32>(at + WINDOW_START, 0.0);
    mem::write::<[f32; 3]>(at + ATTACK_DIRECTION, [0.0; 3]);
    let n = ATTACKS.fetch_add(1, Relaxed);
    if n < 30 {
        log!("physical melee: attack {n} (state {state}) with {held:?}: window from {window:.3} s -> 0, direction {direction:.2?} -> the blade's");
    }
}

unsafe extern "C" fn reach(controller: *mut c_void, weapon: *mut c_void) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let game = unsafe { REACH_ORIGINAL.get()(controller, weapon) };
    let Some((held, length)) = player_melee(controller).and_then(|held| melee_hit::blade(held).map(|length| (held, length))) else { return game };
    let ours = ((length - REACH_EXTRA) / REACH_SCALE).max(0.01);
    let n = REACHES.fetch_add(1, Relaxed);
    if log::seldom(n, 9) {
        log!("physical melee: reach {game:.2} m -> {ours:.2} m (a {length} m blade with {held:?})");
    }
    ours
}
