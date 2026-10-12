//! Physical melee (`physical_melee`, with the hand rig): a swing hits where the weapon in the
//! player's hand passes through an enemy, from the way it moved. DL1's own hit detection already
//! sweeps a blade along the arms' `R_HandHolder` element frame to frame and tests it against each
//! enemy's bones, and the hand rig poses that element; three things in it are made for the canned
//! attack animation instead:
//!
//! - it sweeps only in a window the attack sets up at its start (about 290 ms after the press, for
//!   about 260 ms), by when a real swing is over: the window here opens at the attack's start;
//! - the hit's direction is the attack animation's (a fixed sideways direction per attack type),
//!   not the blade's motion: that override is cleared, so the game keeps the direction the hand
//!   moved;
//! - the blade reaches 1.3 times the weapon's range plus 0.1 m (2.4 m for a machete): here it is
//!   the length of the weapon in the hand ([`Physical::blade`]), or a fist's reach.
//!
//! A swing still presses the attack ([`monaka_arms::melee`]); the game's damage, wounds,
//! reactions, stamina and sounds are its own. With `melee_on_reach` (on by default) a weapon's
//! swing presses it only once the blade is about to reach something the game's melee can hit
//! ([`reaches`]): a wind-up or a swing at the air starts no attack. With `back_is_blunt` (on by
//! default) a cutting weapon's hit lands as a blunt one when the blade did not lead with its edge
//! ([`take_damage`]): the back of an axe bludgeons.

use crate::{engine, player::hands};
use monaka_arms::melee::{Held, Physical};
use eng_chr::melee_reach::ReachLook;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// Sets up an attack: `(controller, attack type)`. Writes the attack's direction and its hit
/// window, among much else.
const ATTACK_START: usize = 0xdc9320;
/// The attack types the attack start sets up (bit per type, from its own check).
const ATTACK_TYPES: u32 = 0x62ffff;
/// The blade's reach for this frame: `(controller, weapon) -> metres`, the weapon's range scaled
/// for how far the player looks up or down.
const REACH: usize = 0xdbec70;
/// The attack's direction (player-local); non-zero overrides each hit's own direction.
const ATTACK_DIRECTION: usize = 0x29c;
/// When the attack starts looking for hits, seconds after it starts.
const WINDOW_START: usize = 0x2a8;
/// How far past the reach the hit test extends the blade: `reach * 1.3 + 0.1`.
const REACH_SCALE: f32 = 1.3;
const REACH_EXTRA: f32 = 0.1;

/// The melee controller's update, every game update: `(controller)`.
const UPDATE: usize = 0xdb1d10;
/// The class the game's melee looks for targets of (its broadphase's `FindObjectsInRadius` filter).
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
/// A cutting weapon's edge faces `R_HandHolder`'s +z: the tip moves that way through the game's own
/// swings (left and right alike). A hit cuts when the tip moved at least this share along it.
const EDGE_LEAD: f32 = 0.5;


type AttackStartFn = unsafe extern "C" fn(*mut c_void, i32);
type TakeDamageFn = unsafe extern "C" fn(*mut c_void, *mut u8);
type UpdateFn = unsafe extern "C" fn(*mut c_void);
type ActiveLevelFn = unsafe extern "system" fn(usize) -> usize;
type FindInRadiusFn = unsafe extern "system" fn(usize, *mut Found, *const [f32; 3], f32, usize, bool, *mut i32) -> bool;
type ExtentsFn = unsafe extern "system" fn(usize) -> *const [f32; 6];
type ElementIdFn = unsafe extern "system" fn(usize, *const i8) -> i32;

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

struct Reach {
    active_level: ActiveLevelFn,
    find: FindInRadiusFn,
    extents: ExtentsFn,
    element_id: ElementIdFn,
    world: engine::ElementWorldFn,
    targets: usize,
}

static LOOKUP: OnceLock<Reach> = OnceLock::new();
static FOUND: Mutex<Found> = Mutex::new(Found { items: std::ptr::null_mut(), count: 0, capacity: 0 });
/// The blade's look (`eng_chr::melee_reach`: its sweep, the box test, the swings' check).
static LOOK: ReachLook = ReachLook::new();
static DESCRIBED: AtomicU64 = AtomicU64::new(0);
static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static TAKE_DAMAGE_ORIGINAL: Original<TakeDamageFn> = Original::new();
/// The hand's `R_HandHolder` world matrix at the last two melee updates: [before, latest].
static HAND_FRAMES: Mutex<[Option<[f32; 12]>; 2]> = Mutex::new([None, None]);
static BLUNTED: AtomicU64 = AtomicU64::new(0);
static DAMAGE_SEEN: AtomicU64 = AtomicU64::new(0);
/// Object classes (by vtable) the reach look leaves out: the game's invisible AI target markers,
/// some of them with a body-sized box.
static MARKERS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
static EDGED: AtomicU64 = AtomicU64::new(0);
type ReachFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> f32;

static ATTACK_START_ORIGINAL: Original<AttackStartFn> = Original::new();
static REACH_ORIGINAL: Original<ReachFn> = Original::new();

static PHYSICAL: OnceLock<Physical> = OnceLock::new();
static ATTACKS: AtomicU64 = AtomicU64::new(0);
static REACHES: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module, physical: Physical, on_reach: bool, back_is_blunt: bool) -> Result<(), Rejection> {
    let _ = PHYSICAL.set(physical);
    if back_is_blunt {
        let target = engine_module.export(TAKE_DAMAGE).ok_or_else(|| Rejection::revision("the engine does not export IControlObject::TakeDamage"))?;
        // SAFETY: the export takes the victim and its damage info (its mangled name); the prologue
        // is decoded and moved.
        unsafe { hooks.inline_decoded(&TAKE_DAMAGE_ORIGINAL, "IControlObject::TakeDamage", target, take_damage as TakeDamageFn)? };
        log!("physical melee: a cutting weapon's hit that does not lead with the edge lands blunt");
    }
    if on_reach || back_is_blunt {
        let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
        // SAFETY: each export has the signature its mangled name states, on x64.
        let resolved = unsafe {
            Reach {
                active_level: std::mem::transmute::<usize, ActiveLevelFn>(find(ACTIVE_LEVEL)?),
                find: std::mem::transmute::<usize, FindInRadiusFn>(find(FIND_IN_RADIUS)?),
                extents: std::mem::transmute::<usize, ExtentsFn>(find(EXTENTS)?),
                element_id: std::mem::transmute::<usize, ElementIdFn>(find(crate::engine::ELEMENT_ID)?),
                world: std::mem::transmute::<usize, engine::ElementWorldFn>(find(engine::ELEMENT_WORLD)?),
                targets: gamedll.at(TARGETS),
            }
        };
        let _ = LOOKUP.set(resolved);
        // SAFETY: the controller's update takes the controller (read from its code); the game
        // DLL's build is checked by the caller; the prologue is decoded and moved.
        unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "melee controller update", gamedll.at(UPDATE), update as UpdateFn)? };
        if on_reach {
            monaka_arms::melee::set_reach_check(reaches);
            log!("physical melee: a weapon's swing attacks once the blade is about to reach something");
        }
    }
    // SAFETY: the detours have the targets' signatures (read from their code and call sites);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&ATTACK_START_ORIGINAL, "melee attack start", gamedll.at(ATTACK_START), attack_start as AttackStartFn)?;
        hooks.inline_decoded(&REACH_ORIGINAL, "melee reach", gamedll.at(REACH), reach as ReachFn)?;
    }
    physical.apply();
    Ok(())
}

fn reaches() -> bool {
    LOOK.reaches()
}

unsafe extern "C" fn update(controller: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller) };
    if let Some(held) = player_melee(controller).filter(|h| matches!(h, Held::Melee | Held::Blade)) {
        look(held);
    }
}

/// On the game's thread, in the player's melee update: whether the blade, where it is and where it
/// will be [`LOOKAHEAD`] from now at its speed, touches the box of anything the game's melee can hit.
fn look(held: Held) {
    let (Some(reach), Some(physical)) = (LOOKUP.get(), PHYSICAL.get()) else { return };
    let model = hands::arms_model();
    let Some(game) = eng_chr::game::game().filter(|_| model != 0) else { return };
    // SAFETY: the engine's own getters on the live arms model, on the game's thread.
    let element = unsafe { (reach.element_id)(model, c"R_HandHolder".as_ptr()) };
    if element < 0 {
        return;
    }
    // SAFETY: as above, a valid element of that model.
    let Some(m) = mem::read::<[f32; 12]>(unsafe { (reach.world)(model as *mut c_void, element) } as usize) else { return };
    if let Ok(mut frames) = HAND_FRAMES.lock() {
        *frames = [frames[1], Some(m)];
    }
    let palm = [m[3], m[7], m[11]];
    let length = physical.reach(held);
    let tip = [0, 1, 2].map(|k| palm[k] + [m[1], m[5], m[9]][k] * length);
    let Some(sweep) = LOOK.sweep(palm, tip) else { return };
    let looks = LOOK.counted();
    let (centre, spread) = (sweep.centre, sweep.spread);
    // An object is found by its own place (its feet), so the radius takes in a body's height.
    let radius = spread + 2.0;
    // SAFETY: the engine's own call on the live game object, on the game's thread.
    let level = unsafe { (reach.active_level)(game) };
    if level == 0 {
        return;
    }
    let Ok(mut found) = FOUND.lock() else { return };
    found.count = 0;
    // SAFETY: the engine's search on its live level; `found` is a vector of its kind, which it
    // grows with its own allocator; the filter is the class the game's melee passes.
    unsafe { (reach.find)(level, &mut *found, &centre, radius, reach.targets, false, std::ptr::null_mut()) };
    let player = model;
    // Diagnostic: now and then, everything the search found and where the blade is.
    let describe = looks.is_multiple_of(200) && DESCRIBED.fetch_add(1, Relaxed) < 40;
    if describe {
        log!("physical melee look {looks}: blade palm {palm:.2?} tip {tip:.2?}, search at {centre:.2?} radius {radius:.2}, {} found", found.count);
    }
    for i in 0..found.count as usize {
        let Some(object) = mem::read::<usize>(found.items as usize + i * 8).filter(|&o| o != 0) else { continue };
        let owner = object - CONTROL_OBJECT_IN_OBJECT;
        if owner == player || object == player {
            if describe {
                log!("  found {i}: the player ({owner:#x})");
            }
            continue;
        }
        // SAFETY: the engine's getter on a live control object.
        let b = mem::read::<[f32; 6]>(unsafe { (reach.extents)(object) } as usize);
        if describe {
            let class = monaka_hook::probe::class_name(owner).unwrap_or_else(|| "?".into());
            log!("  found {i}: {class} at {owner:#x} (control {object:#x}), box {b:.2?}");
        }
        // Not the game's "can be hit" virtual (+0x228): its melee asks it of props, and it says no
        // for characters, which it tests bone by bone instead. (The invisible AI target markers the
        // search also finds have 3 cm boxes, or a body's.)
        if let Some(b) = b.filter(|b| sweep.touches(b))
            && !is_marker(owner)
        {
            LOOK.reached(|| format!("{} ({owner:#x}, box {b:.2?})", monaka_hook::probe::class_name(owner).unwrap_or_else(|| "?".into())));
            break;
        }
    }
    if looks == 0 || looks.is_power_of_two() && looks >= 1024 {
        log!("physical melee: {looks} looks for what the blade reaches; {} objects near the latest", found.count);
    }
}

/// Whether `object` is one of the game's AI target markers (`NightFakeAITarget`,
/// `SoudFakeAITarget`, `FakeAITarget`), by its class, looked up once per vtable.
fn is_marker(object: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(object) else { return false };
    let Ok(mut known) = MARKERS.lock() else { return false };
    if let Some(&(_, marker)) = known.iter().find(|(v, _)| *v == vtable) {
        return marker;
    }
    let marker = monaka_hook::probe::class_name(object).is_some_and(|c| c.contains("FakeAITarget"));
    if known.len() < 256 {
        known.push((vtable, marker));
    }
    marker
}

/// Which way the blade's tip moved over the last melee update, in the hand's own axes (x, y along
/// the blade, z the edge's side), as a unit vector.
fn blade_lead() -> Option<[f32; 3]> {
    let [Some(before), Some(now)] = *HAND_FRAMES.lock().ok()? else { return None };
    let tip = |m: &[f32; 12]| [m[3] + m[1], m[7] + m[5], m[11] + m[9]];
    let (a, b) = (tip(&before), tip(&now));
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if length.is_nan() || length <= 0.002 {
        return None;
    }
    Some([0, 1, 2].map(|c| (d[0] * now[c] + d[1] * now[4 + c] + d[2] * now[8 + c]) / length))
}

/// Every hit: the player's cut that led with the back or a flat of the blade lands blunt.
unsafe extern "C" fn take_damage(victim: *mut c_void, info: *mut u8) {
    let _flight = InFlight::enter();
    let at = info as usize;
    let model = hands::arms_model();
    let ours = model != 0 && !info.is_null() && mem::read::<usize>(at + INFO_ATTACKER) == Some(model + ATTACKER_IN_PLAYER);
    // Diagnostic: the first hits on characters, with what the rule reads.
    if !info.is_null() && monaka_hook::probe::class_name(victim as usize).is_some_and(|c| c.contains("HumanAI")) && DAMAGE_SEEN.fetch_add(1, Relaxed) < 30 {
        log!(
            "physical melee: damage to {victim:p}: attacker {:#x} (the player's would be {:#x}), type {}, holding {:?}, blade motion in the hand {:.2?}",
            mem::read::<usize>(at + INFO_ATTACKER).unwrap_or(0),
            model + ATTACKER_IN_PLAYER,
            mem::read::<i32>(at + INFO_DAMAGE_TYPE).unwrap_or(-1),
            monaka_arms::melee::held(),
            blade_lead(),
        );
    }
    if ours && mem::read::<i32>(at + INFO_DAMAGE_TYPE) == Some(DAMAGE_CUT) && monaka_arms::melee::held().is_some_and(|h| matches!(h, Held::Melee | Held::Blade)) {
        match blade_lead() {
            Some(lead) if lead[2] < EDGE_LEAD => {
                // The game's own damage info for this hit, on its thread, before the victim reads it.
                mem::write::<i32>(at + INFO_DAMAGE_TYPE, DAMAGE_BLUNT);
                let n = BLUNTED.fetch_add(1, Relaxed) + 1;
                if n <= 40 {
                    log!("physical melee: hit {n} led with the {} (blade motion in the hand {lead:.2?}): cut -> blunt", if lead[2] <= -EDGE_LEAD { "back" } else { "flat" });
                }
            }
            lead => {
                let n = EDGED.fetch_add(1, Relaxed) + 1;
                if n <= 40 {
                    log!("physical melee: edge hit {n} (blade motion in the hand {lead:.2?}): a cut");
                }
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { TAKE_DAMAGE_ORIGINAL.get()(victim, info) }
}

/// The player's melee controller, with what the arms hold: the controller's player is the arms
/// model the rig poses, and the arms hold no gun.
fn player_melee(controller: *mut c_void) -> Option<Held> {
    let held = monaka_arms::melee::held().filter(|&h| h != Held::Gun)?;
    hands::players(controller as usize).then_some(held)
}

unsafe extern "C" fn attack_start(controller: *mut c_void, attack: i32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { ATTACK_START_ORIGINAL.get()(controller, attack) };
    // The game returns at once for anything but these attack types.
    if !(0..=0x16).contains(&attack) || (ATTACK_TYPES >> attack) & 1 == 0 {
        return;
    }
    let Some(held) = player_melee(controller) else { return };
    let at = controller as usize;
    let window = mem::read::<f32>(at + WINDOW_START).unwrap_or(f32::NAN);
    // No window set up: nothing to open (as in DL2 and The Beast).
    if !(window >= 0.0) {
        return;
    }
    let direction = mem::read::<[f32; 3]>(at + ATTACK_DIRECTION).unwrap_or([f32::NAN; 3]);
    // The game's own fields of its own controller, written on the game thread inside its call.
    mem::write::<f32>(at + WINDOW_START, 0.0);
    mem::write::<[f32; 3]>(at + ATTACK_DIRECTION, [0.0; 3]);
    let n = ATTACKS.fetch_add(1, Relaxed);
    if n < 30 {
        log!("physical melee: attack {n} type {attack} with {held:?}: window from {window:.3} s -> 0, direction {direction:.2?} -> the blade's");
    }
}

unsafe extern "C" fn reach(controller: *mut c_void, weapon: *mut c_void) -> f32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let game = unsafe { REACH_ORIGINAL.get()(controller, weapon) };
    let (Some(held), Some(physical)) = (player_melee(controller), PHYSICAL.get()) else { return game };
    let length = physical.reach(held);
    let ours = ((length - REACH_EXTRA) / REACH_SCALE).max(0.01);
    let n = REACHES.fetch_add(1, Relaxed);
    if n < 10 || n.is_power_of_two() {
        log!("physical melee: reach {game:.2} m -> {ours:.2} m (a {length} m blade with {held:?})");
    }
    ours
}
