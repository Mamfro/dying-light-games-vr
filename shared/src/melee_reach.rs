//! Attack on reach (with physical melee and the hand rig): a weapon's swing presses the attack only
//! once the blade is about to reach something the game's melee can hit
//! ([`monaka_arms::melee::set_reach_check`]); a wind-up or a swing at the air starts no attack and
//! costs no stamina. Fists swing as before.
//!
//! After the rig places the arms ([`look`]), the blade runs from the arms' `R_HandHolder` along the
//! held weapon's own blade (from its model's root toward its `DTrailStart1`, the point the game's
//! hit test sweeps toward, taken in the model's frame and carried by the placed hand), the weapon's
//! length long ([`crate::melee::blade`]). Where it is and where it will be [`LOOKAHEAD`] on at its
//! speed is tested against the boxes of what the engine's level finds near it
//! (`ILevel::FindObjectsInRadius`, with the class the game's own combat target search passes),
//! leaving out the player and boxes too small for a body. Without a look lately (no weapon model,
//! no level) every swing attacks, as without this.

use crate::model::Model;
use monaka_arms::Skeleton;
use monaka_arms::melee::Held;
use monaka_core::camera::Mat34;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// One build's game DLL globals (RVAs).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The game object (`IGame*`).
    pub game: usize,
    /// The class the game's combat target search finds objects of (`CRTTI*`, set as the game runs).
    pub target_class: usize,
}

const ACTIVE_LEVEL: &str = "?GetActiveLevel@IGame@@QEAAPEAVILevel@@XZ";
const FIND_IN_RADIUS: &str = "?FindObjectsInRadius@ILevel@@QEAA_NPEAV?$vector@PEAVIControlObject@@V?$heap_allocator@PEAVIControlObject@@@vector_allocators@ttl@@$00@ttl@@AEBVvec3@@MPEBVCRTTI@@_NPEAH@Z";
const EXTENTS: &str = "?GetExtentsInWorld@IControlObject@@QEBAAEBVextents@@XZ";
/// The arms' hand holder, and the weapon model's point its blade sweeps toward.
const HAND: &str = "r_handholder";
const BLADE_TIP: &str = "dtrailstart1";
/// How far ahead the blade's path is looked at (seconds): the press, the game's next update and
/// the attack's start all come before the attack can hit.
const LOOKAHEAD: f32 = 0.15;
/// How near an object's box the blade counts as reaching it (metres).
const MARGIN: f32 = 0.1;
/// The least height of a box that counts (metres): a body's, lying or standing.
const LEAST_HEIGHT: f32 = 0.4;
/// A reach seen within this is current (ms); the check answers yes to anything older than
/// [`STALE_MS`] since the last look (the look not running: every swing attacks).
const REACH_FRESH_MS: u64 = 60;
const STALE_MS: u64 = 300;
/// An object is found by its own place (its feet): the search's radius takes in a body's height.
const BODY: f32 = 2.0;

type ActiveLevelFn = unsafe extern "system" fn(usize) -> usize;
type FindInRadiusFn = unsafe extern "system" fn(usize, *mut Found, *const [f32; 3], f32, usize, bool, *mut i32) -> bool;
type ExtentsFn = unsafe extern "system" fn(usize) -> *const [f32; 6];

struct Lookup {
    active_level: ActiveLevelFn,
    find: FindInRadiusFn,
    extents: ExtentsFn,
    game: usize,
    target_class: usize,
}

static LOOKUP: OnceLock<Lookup> = OnceLock::new();

/// The engine's compact `ttl::vector<IControlObject*, heap_allocator, 1>` the search fills: the data
/// in the header's low 48 bits, its built-in capacity in byte 6, and the count in byte 7 (less one)
/// while the data is the built-in slot, else at +8 (the capacity at +12). Set up as the game does,
/// on its one built-in slot; grown by the engine with its own allocator, kept, never freed.
#[repr(C)]
struct Found {
    header: u64,
    slot: u64,
}

// SAFETY: only used on the game's thread, under its mutex.
unsafe impl Send for Found {}

impl Found {
    /// Empty on its built-in slot, unless the engine moved it to its own storage already.
    fn ready(&mut self) {
        if self.header == 0 {
            let slot = &raw mut self.slot as u64;
            self.header = slot | 1 << 48 | 1 << 56;
        }
    }

    fn items(&self) -> Vec<usize> {
        let data = (self.header & 0xffff_ffff_ffff) as usize;
        let small = (self.header >> 56) as u8;
        let count = if small != 0 { small as usize - 1 } else { (self.slot & 0xffff_ffff) as usize };
        if data == 0 || count > 1024 {
            return Vec::new();
        }
        (0..count).filter_map(|i| mem::read::<usize>(data + i * 8)).filter(|&o| o != 0).collect()
    }
}

static FOUND: Mutex<Found> = Mutex::new(Found { header: 0, slot: 0 });
/// The blade's hand end and tip at a look, and when.
type Blade = ([f32; 3], [f32; 3], Instant);
/// The blade at the previous look.
static LAST_BLADE: Mutex<Option<Blade>> = Mutex::new(None);
/// When the last look ran, and when it last found the blade about to reach something (ticks).
static LOOKED: AtomicU64 = AtomicU64::new(0);
static REACHED: AtomicU64 = AtomicU64::new(0);
static LOOKS: AtomicU64 = AtomicU64::new(0);
static DESCRIBED: AtomicU64 = AtomicU64::new(0);
/// The skeleton whose hand holder is known, and that element.
static HAND_OF: Mutex<Option<(usize, Option<i32>)>> = Mutex::new(None);
/// Object classes (by vtable) the look leaves out (the player, the AI's target markers).
static LEFT_OUT: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
static NO_TIP: AtomicUsize = AtomicUsize::new(0);
static BLADE_LOGGED: AtomicUsize = AtomicUsize::new(0);

/// Resolves the engine's calls and has the swings ask [`reaches`].
pub fn install(engine: &Module, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let find = |name: &str| engine.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let lookup = unsafe {
        Lookup {
            active_level: std::mem::transmute::<usize, ActiveLevelFn>(find(ACTIVE_LEVEL)?),
            find: std::mem::transmute::<usize, FindInRadiusFn>(find(FIND_IN_RADIUS)?),
            extents: std::mem::transmute::<usize, ExtentsFn>(find(EXTENTS)?),
            game: gamedll.at(build.game),
            target_class: gamedll.at(build.target_class),
        }
    };
    let _ = LOOKUP.set(lookup);
    monaka_arms::melee::set_reach_check(reaches);
    log!("physical melee: a weapon's swing attacks once the blade is about to reach something");
    Ok(())
}

/// Whether the blade was about to reach something at the latest look (or the look is not running).
fn reaches() -> bool {
    let now = monaka_channel::tick();
    now.saturating_sub(LOOKED.load(Relaxed)) > STALE_MS || now.saturating_sub(REACHED.load(Relaxed)) <= REACH_FRESH_MS
}

fn unit(v: [f32; 3]) -> Option<[f32; 3]> {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (l.is_finite() && l > 1e-6).then(|| v.map(|c| c / l))
}

/// A world direction in `m`'s terms (its axes made unit).
fn into(m: &Mat34, v: [f32; 3]) -> Option<[f32; 3]> {
    let axes = [0, 1, 2].map(|c| unit([m[c], m[4 + c], m[8 + c]]));
    let [Some(x), Some(y), Some(z)] = axes else { return None };
    Some([x, y, z].map(|a| a[0] * v[0] + a[1] * v[1] + a[2] * v[2]))
}

/// A direction in `m`'s terms back in the world (its axes made unit).
fn out_of(m: &Mat34, v: [f32; 3]) -> Option<[f32; 3]> {
    let axes = [0, 1, 2].map(|c| unit([m[c], m[4 + c], m[8 + c]]));
    let [Some(x), Some(y), Some(z)] = axes else { return None };
    Some([0, 1, 2].map(|k| x[k] * v[0] + y[k] * v[1] + z[k] * v[2]))
}

/// The arms' hand holder in `skeleton` (found once per skeleton, by name without case).
fn hand(skeleton: &impl Skeleton) -> Option<i32> {
    let mut known = HAND_OF.lock().ok()?;
    if let Some((id, element)) = *known
        && id == skeleton.id()
    {
        return element;
    }
    let element = (0..skeleton.count()).find(|&e| skeleton.name(e).eq_ignore_ascii_case(HAND));
    *known = Some((skeleton.id(), element));
    element
}

/// The weapon's blade in its model's frame (from the root toward `DTrailStart1`), unit.
fn blade_in_model(weapon: usize) -> Option<[f32; 3]> {
    let model = Model::of(weapon)?;
    let Some(tip) = (0..model.count()).find(|&e| model.name(e).eq_ignore_ascii_case(BLADE_TIP)) else {
        if NO_TIP.swap(weapon, Relaxed) != weapon {
            log!("physical melee: the held weapon {} has no {BLADE_TIP}: its swings attack as before", class_name(weapon).unwrap_or_default());
        }
        return None;
    };
    let (root, at) = (model.world(0)?, model.world(tip)?);
    let along = unit(into(&root, [at[3] - root[3], at[7] - root[7], at[11] - root[11]])?)?;
    if BLADE_LOGGED.swap(weapon, Relaxed) != weapon {
        log!("physical melee: {}'s blade runs {along:.3?} in its model (from the root to {BLADE_TIP})", class_name(weapon).unwrap_or_default());
    }
    Some(along)
}

/// After the rig placed the arms (`skeleton`) holding `weapon` (its visual) as `held`: whether the
/// blade, where it is and where it will be [`LOOKAHEAD`] from now, touches the box of anything the
/// game's melee can hit.
pub fn look(skeleton: &impl Skeleton, weapon: usize, held: Held) {
    let Some(lookup) = LOOKUP.get() else { return };
    if !matches!(held, Held::Melee | Held::Blade) || weapon == 0 {
        return;
    }
    let (Some(holder), Some(length)) = (hand(skeleton).and_then(|e| skeleton.world(e)), crate::melee::blade(held)) else { return };
    let Some(along) = blade_in_model(weapon).and_then(|d| out_of(&holder, d)) else { return };
    let palm = [holder[3], holder[7], holder[11]];
    let tip = [0, 1, 2].map(|k| palm[k] + along[k] * length);
    if !palm.iter().chain(&tip).all(|v| v.is_finite()) {
        return;
    }
    let now = Instant::now();
    let previous = LAST_BLADE.lock().ok().and_then(|mut last| last.replace((palm, tip, now)));
    // Where it will be: moving on as it moved since the last look.
    let (palm_ahead, tip_ahead) = match previous {
        Some((p0, t0, then)) if (now - then).as_secs_f32() > 0.0 && (now - then).as_secs_f32() < 0.1 => {
            let scale = LOOKAHEAD / (now - then).as_secs_f32();
            ([0, 1, 2].map(|k| palm[k] + (palm[k] - p0[k]) * scale), [0, 1, 2].map(|k| tip[k] + (tip[k] - t0[k]) * scale))
        }
        _ => (palm, tip),
    };
    let Some(game) = mem::read::<usize>(lookup.game).filter(|&g| g != 0) else { return };
    // SAFETY: the engine's own getter on the live game object, on the game's thread.
    let level = unsafe { (lookup.active_level)(game) };
    if level == 0 {
        return;
    }
    LOOKED.store(monaka_channel::tick(), Relaxed);
    let looks = LOOKS.fetch_add(1, Relaxed);
    // Points along the blade, now and ahead.
    let mut points = Vec::with_capacity(15);
    for when in [0.0f32, 0.5, 1.0] {
        let p = [0, 1, 2].map(|k| palm[k] + (palm_ahead[k] - palm[k]) * when);
        let t = [0, 1, 2].map(|k| tip[k] + (tip_ahead[k] - tip[k]) * when);
        for share in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            points.push([0, 1, 2].map(|k| p[k] + (t[k] - p[k]) * share));
        }
    }
    let centre = [0, 1, 2].map(|k| points.iter().map(|p| p[k]).sum::<f32>() / points.len() as f32);
    let spread = points.iter().map(|p| monaka_core::math::distance(*p, centre)).fold(0.0f32, f32::max);
    let class = mem::read::<usize>(lookup.target_class).unwrap_or(0);
    let Ok(mut found) = FOUND.lock() else { return };
    found.ready();
    // SAFETY: the engine's search on its live level, on the game's thread; `found` is a vector of
    // its kind, set up as the game sets up its own; the filter is the class the game's own search
    // passes (or none, before the game set it).
    unsafe { (lookup.find)(level, &mut *found, &centre, spread + BODY, class, false, std::ptr::null_mut()) };
    let objects = found.items();
    drop(found);
    let describe = looks.is_multiple_of(200) && DESCRIBED.fetch_add(1, Relaxed) < 30;
    if describe {
        log!("physical melee look {looks}: blade {palm:.2?} to {tip:.2?}, {} found near {centre:.2?} (class {class:#x})", objects.len());
    }
    let mut hit = None;
    for object in objects {
        // SAFETY: the engine's getter on a live control object it just found.
        let b = mem::read::<[f32; 6]>(unsafe { (lookup.extents)(object) } as usize);
        if describe {
            log!("  {} at {object:#x}, box {b:.2?}", class_name(object).unwrap_or_else(|| "?".into()));
        }
        let Some(b) = b.filter(|b| b.iter().all(|v| v.is_finite()) && b[4] - b[1] >= LEAST_HEIGHT) else { continue };
        if left_out(object) {
            continue;
        }
        let inside = |p: &[f32; 3]| (0..3).all(|k| p[k] >= b[k] - MARGIN && p[k] <= b[k + 3] + MARGIN);
        if points.iter().any(inside) {
            hit = Some((object, b));
            break;
        }
    }
    if let Some((object, b)) = hit {
        let before = REACHED.swap(monaka_channel::tick(), Relaxed);
        if monaka_channel::tick().saturating_sub(before) > 500 && looks < 100_000 {
            log!("physical melee: the blade is about to reach {} ({object:#x}, box {b:.2?})", class_name(object).unwrap_or_else(|| "?".into()));
        }
    }
}

/// Whether `object` is the player or one of the AI's target markers, by its class (once per vtable).
fn left_out(object: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(object) else { return true };
    let Ok(mut known) = LEFT_OUT.lock() else { return true };
    if let Some(&(_, out)) = known.iter().find(|(v, _)| *v == vtable) {
        return out;
    }
    let name = class_name(object).unwrap_or_default();
    let out = name.contains("Player") || name.contains("FakeAITarget");
    if known.len() < 256 {
        known.push((vtable, out));
    }
    out
}

pub fn report() {
    let looks = LOOKS.load(Relaxed);
    if looks > 0 {
        log!("physical melee: {looks} looks for what the blade reaches");
    }
}
