//! `probe_sweep`: what the player's melee hit detection sees.
//! Each frame of an attack window, `WeaponMeleeController` sweeps a
//! blade along the `R_HandHolder` element of the object its virtual +0x20 returns; this logs that
//! object against the arms model the hand rig poses, where each puts `R_HandHolder`, and whether
//! the rig ran since the last sweep. Each hit the sweep turns into damage is logged with its
//! point, direction and bone. Read-only.

use crate::{engine, player::hands};
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::time::Instant;

/// One frame of hit detection for an attack track: `(controller, track, mode)`.
const DETECT: usize = 0xdb4410;
/// Builds the damage info from a hit entry and deals it: `(out, attacker, entry, weapon, flags,
/// damage multiplier, force, extra)`.
const DEAL: usize = 0xc4d920;
const ELEMENT_ID: &str = "?GetElementID@IModelObject@@QEBAHPEBD@Z";
/// The controller's tracks, as offsets into it.
const TRACKS: [(usize, &str); 2] = [(0x6f0, "first"), (0x728, "second")];
/// The controller's attack type.
const ATTACK_TYPE: usize = 0x110;
/// The controller's attack direction (player-local), which overrides a hit's own direction.
const ATTACK_DIRECTION: usize = 0x29c;
/// Lines logged per kind before the probe goes quiet.
const LINES: u64 = 400;

type DetectFn = unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> usize;
type DealFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, *mut c_void, u32, f32, f32, *mut c_void) -> usize;
type GetterFn = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
type ElementIdFn = unsafe extern "system" fn(*mut c_void, *const i8) -> i32;

static DETECT_ORIGINAL: Original<DetectFn> = Original::new();
static DEAL_ORIGINAL: Original<DealFn> = Original::new();

struct Engine {
    element_id: ElementIdFn,
    world: engine::ElementWorldFn,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();
static SWEEPS: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
/// The rig's call count at the previous sweep.
static RIG_AT_SWEEP: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let resolved = unsafe {
        Engine {
            element_id: std::mem::transmute::<usize, ElementIdFn>(find(ELEMENT_ID)?),
            world: std::mem::transmute::<usize, engine::ElementWorldFn>(find(engine::ELEMENT_WORLD)?),
        }
    };
    let _ = ENGINE.set(resolved);
    STARTED.get_or_init(Instant::now);
    // SAFETY: the detours have the targets' signatures (from their call sites); the game DLL's
    // build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&DETECT_ORIGINAL, "melee hit detection", gamedll.at(DETECT), detect as DetectFn)?;
        hooks.inline_decoded(&DEAL_ORIGINAL, "melee hit dealing", gamedll.at(DEAL), deal as DealFn)?;
    }
    log!("sweep probe: watching the melee controller's hit detection");
    Ok(())
}

fn ms() -> u128 {
    STARTED.get().map_or(0, |s| s.elapsed().as_millis())
}

/// Where `model` has its `R_HandHolder` (translation; the blade runs along its column 1).
fn hand_holder(model: usize) -> Option<([f32; 3], [f32; 3])> {
    let engine = ENGINE.get()?;
    if model == 0 {
        return None;
    }
    let name = c"R_HandHolder";
    // SAFETY: a live IModelObject on the game thread; the engine returns -1 for a missing name.
    let element = unsafe { (engine.element_id)(model as *mut c_void, name.as_ptr()) };
    if element < 0 {
        return None;
    }
    // SAFETY: a valid element index of that model.
    let matrix = mem::read::<[f32; 12]>(unsafe { (engine.world)(model as *mut c_void, element) } as usize)?;
    Some(([matrix[3], matrix[7], matrix[11]], [matrix[1], matrix[5], matrix[9]]))
}

/// `model`'s `R_HandHolder` world matrix (row-major 3x4: columns 0..2 the element's axes).
fn hand_frame(model: usize) -> Option<[f32; 12]> {
    let engine = ENGINE.get()?;
    if model == 0 {
        return None;
    }
    // SAFETY: a live IModelObject on the game thread; the engine returns -1 for a missing name.
    let element = unsafe { (engine.element_id)(model as *mut c_void, c"R_HandHolder".as_ptr()) };
    if element < 0 {
        return None;
    }
    // SAFETY: a valid element index of that model.
    mem::read::<[f32; 12]>(unsafe { (engine.world)(model as *mut c_void, element) } as usize)
}

/// The hand frame at the previous sweep, and the tip's motion summed in the hand's own axes
/// (each frame's motion as a unit vector, so every frame counts alike): which side of the weapon
/// leads in the game's own swings, its edge.
static LAST_FRAME: std::sync::Mutex<Option<[f32; 12]>> = std::sync::Mutex::new(None);
static LEAD: std::sync::Mutex<([f32; 3], u64)> = std::sync::Mutex::new(([0.0; 3], 0));

/// The tip's motion since the previous sweep in the hand's axes (x, y along the blade, z), unit.
fn tip_motion(model: usize) -> Option<[f32; 3]> {
    let now = hand_frame(model)?;
    let before = LAST_FRAME.lock().ok()?.replace(now)?;
    // A point a metre out the blade (local +y).
    let tip = |m: &[f32; 12]| [m[3] + m[1], m[7] + m[5], m[11] + m[9]];
    let (a, b) = (tip(&before), tip(&now));
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if !(length > 0.002) {
        return None;
    }
    let axis = |c: usize| [now[c], now[4 + c], now[8 + c]];
    let local = [0, 1, 2].map(|c| {
        let k = axis(c);
        (d[0] * k[0] + d[1] * k[1] + d[2] * k[2]) / length
    });
    if let Ok(mut lead) = LEAD.lock() {
        (0..3).for_each(|k| lead.0[k] += local[k]);
        lead.1 += 1;
    }
    Some(local)
}

fn vec3(at: usize) -> [f32; 3] {
    mem::read::<[f32; 3]>(at).unwrap_or([f32::NAN; 3])
}

unsafe extern "C" fn detect(controller: *mut c_void, track: *mut c_void, mode: i32) -> usize {
    let _flight = InFlight::enter();
    let n = SWEEPS.fetch_add(1, Relaxed);
    if n < LINES {
        let ctrl = controller as usize;
        // The object the sweep reads the hand from, asked the way the sweep asks.
        let object = mem::read::<usize>(ctrl)
            .and_then(|vtable| mem::read::<usize>(vtable + 0x20))
            .filter(|&get| Module::find(engine::GAMEDLL).is_some_and(|m| m.contains(get)))
            // SAFETY: the controller's own getter, on the game thread inside its own update.
            .map_or(0, |get| unsafe { std::mem::transmute::<usize, GetterFn>(get)(controller) } as usize);
        let arms = hands::arms_model();
        let rig = hands::rig_calls();
        let rig_since = rig - RIG_AT_SWEEP.swap(rig, Relaxed);
        let track_name = TRACKS.iter().find(|(o, _)| ctrl + o == track as usize).map_or("?", |(_, name)| name);
        let fmt = |h: Option<([f32; 3], [f32; 3])>| h.map_or("none".to_owned(), |(at, along)| format!("{at:.3?} along {along:.2?}"));
        let motion = tip_motion(object).map_or("none".to_owned(), |m| format!("{m:.2?}"));
        log!(
            "sweep probe: {} ms sweep {n} track {track_name} mode {mode} attack type {} direction {:.2?} | object {object:#x} ({}) {} arms model {arms:#x} | hand {} | arms hand {} | tip motion in the hand's axes {motion} | rig calls since last sweep {rig_since}",
            ms(),
            mem::read::<i32>(ctrl + ATTACK_TYPE).unwrap_or(-1),
            vec3(ctrl + ATTACK_DIRECTION),
            class_name(object).unwrap_or_else(|| "?".into()),
            if object == arms { "IS the" } else { "is not the" },
            fmt(hand_holder(object)),
            fmt(hand_holder(arms)),
        );
    }
    // SAFETY: forwards the game's own call.
    unsafe { DETECT_ORIGINAL.get()(controller, track, mode) }
}

unsafe extern "C" fn deal(
    out: *mut c_void,
    attacker: *mut c_void,
    entry: *const u8,
    weapon: *mut c_void,
    flags: u32,
    multiplier: f32,
    force: f32,
    extra: *mut c_void,
) -> usize {
    let _flight = InFlight::enter();
    let n = HITS.fetch_add(1, Relaxed);
    if n < LINES && !entry.is_null() {
        let at = entry as usize;
        let victim = mem::read::<usize>(at).unwrap_or(0);
        let point = vec3(at + 0x18);
        // How far out the blade the hit is: from the hand element, along it and off it.
        let reach = hand_holder(hands::arms_model()).map_or("hand unknown".to_owned(), |(hand, along)| {
            let d = [0, 1, 2].map(|k| point[k] - hand[k]);
            let out = d[0] * along[0] + d[1] * along[1] + d[2] * along[2];
            let off = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2] - out * out).max(0.0).sqrt();
            format!("{out:.2} m out the blade, {off:.2} m off it")
        });
        log!(
            "sweep probe: {} ms hit {n} on {victim:#x} ({}) element {} bone {} point {:.3?} ({reach}) direction {:.2?} fraction {:.2} flags {flags:#x} multiplier {multiplier} force {force}",
            ms(),
            class_name(victim).unwrap_or_else(|| "?".into()),
            mem::read::<i32>(at + 0x08).unwrap_or(-1),
            mem::read::<i32>(at + 0x30).unwrap_or(-1),
            point,
            vec3(at + 0x0c),
            mem::read::<f32>(at + 0x38).unwrap_or(f32::NAN),
        );
    }
    // SAFETY: forwards the game's own call.
    unsafe { DEAL_ORIGINAL.get()(out, attacker, entry, weapon, flags, multiplier, force, extra) }
}

pub fn report() {
    if STARTED.get().is_some() {
        log!("sweep probe: {} sweeps, {} hits dealt", SWEEPS.load(Relaxed), HITS.load(Relaxed));
        if let Ok(lead) = LEAD.lock()
            && lead.1 > 0
        {
            let n = lead.1 as f32;
            log!("sweep probe: the tip led along the hand's axes on average x {:.2} y {:.2} z {:.2} over {} sweeps (the weapon's edge leads in the game's swings)", lead.0[0] / n, lead.0[1] / n, lead.0[2] / n, lead.1);
        }
    }
}
