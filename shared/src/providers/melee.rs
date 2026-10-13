//! Provider part: melee hits (`monaka_body::melee_hit::Hits`), what Dying Light 2 and The Beast
//! share: their hit detection changed for the player's own controller ([`install_for`]), and what
//! their melee can hit ([`near`]).
//!
//! The game's hit detection sweeps a blade from the arms' `R_HandHolder` toward the weapon's
//! `DTrailStart1`, frame to frame, against each enemy's bones, and the hand rig poses that hand.
//! Three things in it are made for the canned attack animation instead:
//!
//! - the sweep runs only in a window that opens well after the attack starts (DL2: about 180 ms),
//!   by when a real swing is over: here it opens at the attack's start ([`attack_start`]);
//! - even so, the attack starts after the swing that pressed it (The Beast: 50 to 130 ms, more
//!   while the last attack plays out), often once the hand has stopped: so the window's first
//!   sweeps go along the path the hand took from just before that swing, a few posings at a time,
//!   until they catch up with the hand ([`replay`]);
//! - the hit's direction is the attack animation's, through the controller's direction getter:
//!   here that getter returns zero, so each hit keeps the direction the hand moved ([`direction`]);
//! - the blade is the weapon's range long (DL2: 1.49 m), which is kept; a swept segment shorter
//!   than the reach of what is in the hand (`monaka_body::melee_hit::blade`: a fist's) is made
//!   that long ([`test_segments`]).
//!
//! What the melee can hit ([`near`], where the build has been read for it): the objects the
//! engine's level finds near a point (`ILevel::FindObjectsInRadius`, with the class the game's own
//! combat target search passes), the player and the AI's target markers left out.

use crate::feature::Chrome;
use monaka_body::held::{self, Kind};
use monaka_core::camera::{Mat34, apply_3x3, inverse, position};
use monaka_body::melee_hit;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Missing, Start};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Mutex, OnceLock};

/// One build's melee code and controller fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// Sets up an attack: `(controller, attack type)`. Writes the window and the direction override.
    pub attack_start: usize,
    /// The controller interface's direction getter (`IMeleeController` +0x90): `(interface) -> *vec3`.
    pub direction_getter: usize,
    /// Tests the swept segments against the enemies: `(player, interface, &segments, &hits, n)`.
    pub test_segments: usize,
    /// The controller's field for when the sweep starts, seconds after the attack's start.
    pub window_start: usize,
    /// The attack start's copy of it, behind the controller's "time until the hit window": left
    /// alone, the blade is not sampled until then (DL2: an empty track for about 150 ms).
    pub window_start_copy: usize,
    /// The attack types the attack start sets up (bit per type, from its own check).
    pub attack_types: u32,
    /// A swept segment's size: previous start and tip, current start and tip, (The Beast: 16
    /// more bytes,) a flag.
    pub segment: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// One build's game DLL globals for what the melee can hit (RVAs).
#[derive(Clone, Copy, Debug)]
pub struct Targets {
    /// The game object (`IGame*`).
    pub game: usize,
    /// The class the game's combat target search finds objects of (`CRTTI*`, set as the game runs).
    pub target_class: usize,
}

/// The `IMeleeController` interface in the controller.
const INTERFACE: usize = 0x40;
/// The controller's player.
const PLAYER: usize = 0x08;
/// A segment's blades: the start's and the tip's offsets, previous and current.
const BLADES: [(usize, usize); 2] = [(0x00, 0x0c), (0x18, 0x24)];

const ACTIVE_LEVEL: &str = "?GetActiveLevel@IGame@@QEAAPEAVILevel@@XZ";
const FIND_IN_RADIUS: &str = "?FindObjectsInRadius@ILevel@@QEAA_NPEAV?$vector@PEAVIControlObject@@V?$heap_allocator@PEAVIControlObject@@@vector_allocators@ttl@@$00@ttl@@AEBVvec3@@MPEBVCRTTI@@_NPEAH@Z";
const EXTENTS: &str = "?GetExtentsInWorld@IControlObject@@QEBAAEBVextents@@XZ";

type AttackStartFn = unsafe extern "C" fn(*mut c_void, i32);
type DirectionFn = unsafe extern "C" fn(*mut c_void) -> *const [f32; 3];
type TestSegmentsFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u8, *mut c_void, usize) -> usize;
type ActiveLevelFn = unsafe extern "system" fn(usize) -> usize;
type FindInRadiusFn = unsafe extern "system" fn(usize, *mut Found, *const [f32; 3], f32, usize, bool, *mut i32) -> bool;
type ExtentsFn = unsafe extern "system" fn(usize) -> *const [f32; 6];

static ATTACK_START_ORIGINAL: Original<AttackStartFn> = Original::new();
static DIRECTION_ORIGINAL: Original<DirectionFn> = Original::new();
static TEST_SEGMENTS_ORIGINAL: Original<TestSegmentsFn> = Original::new();

/// What the direction getter returns for the player: no override.
static NO_DIRECTION: [f32; 3] = [0.0; 3];

static BUILD: OnceLock<Build> = OnceLock::new();
/// The player character's vtable, where the game DLL is.
static PLAYER_VTABLE: AtomicUsize = AtomicUsize::new(0);
static ATTACKS: AtomicU64 = AtomicU64::new(0);
static CUTS: AtomicU64 = AtomicU64::new(0);

/// Game `G`'s hit detection hooked (its `Hits::install`).
pub fn install_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let build = G::FACTS.melee.ok_or_else(|| Lack::Missing("the melee code is not read for this build".into()))?;
    let gamedll = crate::feature::gamedll(start)?;
    install(start.hooks, gamedll, build).missing()
}

/// Hooks `build`'s attack start, direction getter and segment test in the game DLL.
fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = BUILD.set(build);
    PLAYER_VTABLE.store(gamedll.at(build.player_vtable), Relaxed);
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&ATTACK_START_ORIGINAL, "melee attack start", gamedll.at(build.attack_start), attack_start as AttackStartFn)?;
        hooks.inline_decoded(&DIRECTION_ORIGINAL, "melee direction", gamedll.at(build.direction_getter), direction as DirectionFn)?;
        hooks.inline_decoded(&TEST_SEGMENTS_ORIGINAL, "melee segments", gamedll.at(build.test_segments), test_segments as TestSegmentsFn)?;
    }
    Ok(())
}

/// What the arms hold, if `player` is the player character and holds no gun.
fn player_melee(player: usize) -> Option<Kind> {
    let held = melee_hit::wielded().filter(|&h| h != Kind::Gun)?;
    let vtable = PLAYER_VTABLE.load(Relaxed);
    (vtable != 0 && mem::read::<usize>(player) == Some(vtable)).then_some(held)
}

/// The same for the controller `ctrl` (its player at +8).
fn controller_melee(ctrl: usize) -> Option<Kind> {
    player_melee(mem::read::<usize>(ctrl + PLAYER)?)
}

unsafe extern "C" fn attack_start(controller: *mut c_void, attack: i32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { ATTACK_START_ORIGINAL.get()(controller, attack) };
    let Some(build) = BUILD.get() else { return };
    if !(0..32).contains(&attack) || (build.attack_types >> attack) & 1 == 0 {
        return;
    }
    let ctrl = controller as usize;
    let Some(held) = controller_melee(ctrl) else { return };
    let window = mem::read::<f32>(ctrl + build.window_start).unwrap_or(f32::NAN);
    if window.is_nan() || window < 0.0 {
        return;
    }
    // The game's own fields of its own controller, written on the game thread inside its call.
    mem::write::<f32>(ctrl + build.window_start, 0.0);
    mem::write::<f32>(ctrl + build.window_start_copy, 0.0);
    arm_replay();
    let n = ATTACKS.fetch_add(1, Relaxed);
    if n < 30 {
        log!("physical melee: attack {n} type {attack} with {held:?}: window from {window:.3} s -> 0");
    }
}

unsafe extern "C" fn direction(interface: *mut c_void) -> *const [f32; 3] {
    let _flight = InFlight::enter();
    if controller_melee((interface as usize).wrapping_sub(INTERFACE)).is_some() {
        return &NO_DIRECTION;
    }
    // SAFETY: forwards the game's own call.
    unsafe { DIRECTION_ORIGINAL.get()(interface) }
}

/// The segments' data and count: a compact vector (data in the first word's low 48 bits; the
/// count in the byte at +7, less one, or in the dword at +8 when that byte is 0).
fn segments(list: usize) -> Option<(usize, usize)> {
    let data = mem::read::<u64>(list)? & 0xffff_ffff_ffff;
    let small = mem::read::<u8>(list + 7)?;
    let count = if small != 0 { small as usize - 1 } else { mem::read::<u32>(list + 8)? as usize };
    (data != 0 && count <= 4096).then_some((data as usize, count))
}

unsafe extern "C" fn test_segments(player: *mut c_void, interface: *mut c_void, list: *mut u8, hits: *mut c_void, n: usize) -> usize {
    let _flight = InFlight::enter();
    let held = player_melee(player as usize);
    if let (Some(_), Some(build), Some((data, count))) = (held, BUILD.get(), segments(list as usize)) {
        replay(data, count, build.segment);
    }
    if let (Some(held), Some(length), Some(build), Some((data, count))) = (held, held.and_then(melee_hit::blade), BUILD.get(), segments(list as usize)) {
        for i in 0..count {
            let segment = data + i * build.segment;
            for (start, tip) in BLADES {
                let (Some(from), Some(to)) = (mem::read_finite::<3>(segment + start), mem::read_finite::<3>(segment + tip)) else { continue };
                let along = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
                let was = (along[0] * along[0] + along[1] * along[1] + along[2] * along[2]).sqrt();
                if was > 1e-4 && length > was {
                    let cut = along.map(|c| c / was * length);
                    // The game's own segment list, on the game thread inside its own call.
                    mem::write(segment + tip, [from[0] + cut[0], from[1] + cut[1], from[2] + cut[2]]);
                    let k = CUTS.fetch_add(1, Relaxed);
                    if log::seldom(k, 4) {
                        log!("physical melee: blade {was:.2} m -> {length} m with {held:?} ({count} segments)");
                    }
                }
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { TEST_SEGMENTS_ORIGINAL.get()(player, interface, list, hits, n) }
}

// ---- The swing replayed ----

/// Posings of the aim hand kept (about 0.6 s at 150 a second).
const TRAIL_LEN: usize = 96;
/// An attack starting at most this long (ms) after a swing is that swing's: its sweeps replay it.
/// Later, it is the game's own (a combo's next blow).
const REPLAY_WITHIN_MS: u64 = 450;
/// The replay starts this long (ms) before the swing was told: a swing is told once the blade is
/// already fast.
const REPLAY_LEAD_MS: u64 = 60;
/// Posings a replayed sweep goes over at once.
const REPLAY_STEP: usize = 3;

/// The aim hand's holder at each posing (`monaka_channel::tick`, the holder in the world), oldest
/// first.
static TRAIL: Mutex<VecDeque<(u64, Mat34)>> = Mutex::new(VecDeque::new());
/// While a swing is replayed: the posing (tick) its next sweep starts at.
static REPLAY: Mutex<Option<u64>> = Mutex::new(None);
/// The swing last replayed (its tick), the swings replayed and the sweeps that replayed them.
static REPLAYED: AtomicU64 = AtomicU64::new(0);
static REPLAYS: AtomicU64 = AtomicU64::new(0);
static REPLAY_SWEEPS: AtomicU64 = AtomicU64::new(0);

/// The hand rig posed the arms (the game's thread): where the aim hand (`side`) holds its item now.
pub fn note_posed<G: Chrome>(side: usize) {
    let Some(posed) = held::in_world::<G>(side) else { return };
    let mut trail = TRAIL.locked();
    if trail.back().is_some_and(|&(at, holder)| at == posed.posed || holder == posed.holder) {
        return;
    }
    if trail.len() == TRAIL_LEN {
        trail.pop_front();
    }
    trail.push_back((posed.posed, posed.holder));
}

/// An attack of the player's started: if a swing pressed it, its sweeps replay that swing; one
/// with no swing of its own (a combo's next blow) sweeps where the hand is.
fn arm_replay() {
    let now = monaka_channel::tick();
    let swing = monaka_body::melee::swung_at().filter(|&at| now.saturating_sub(at) <= REPLAY_WITHIN_MS);
    let mut replay = REPLAY.locked();
    match swing {
        Some(at) if REPLAYED.swap(at, Relaxed) != at => {
            *replay = Some(at.saturating_sub(REPLAY_LEAD_MS));
            let n = REPLAYS.fetch_add(1, Relaxed);
            if log::seldom(n, 8) {
                log!("physical melee: the attack started {} ms after its swing: its sweeps go along the hand's path from {REPLAY_LEAD_MS} ms before the swing", now - at);
            }
        }
        // The same swing's attack, set up again: its replay goes on.
        Some(_) => {}
        None => *replay = None,
    }
}

/// `p` (in the frame of `m`) in the world.
fn through(m: &Mat34, p: [f32; 3]) -> [f32; 3] {
    let (r, t) = (apply_3x3(m, p), position(m));
    [r[0] + t[0], r[1] + t[1], r[2] + t[2]]
}

/// While a swing is replayed, each of the `count` segments at `data` (`size` bytes each) goes
/// along the hand's path instead: the game's own blade (its current start and tip, taken into the
/// latest posing's holder) where the holder was [`REPLAY_STEP`] posings apart, from where the last
/// sweep left off, until the newest posing.
fn replay(data: usize, count: usize, size: usize) {
    let mut replay = REPLAY.locked();
    let Some(next) = *replay else { return };
    let trail = TRAIL.locked();
    let from = trail.iter().position(|&(at, _)| at >= next).filter(|&i| i + 1 < trail.len());
    let (Some(from), Some(back)) = (from, trail.back().and_then(|(_, now)| inverse(now))) else {
        *replay = None;
        return;
    };
    let to = (from + REPLAY_STEP).min(trail.len() - 1);
    let [(_, before), (_, after)] = [trail[from], trail[to]];
    let [(start_was, tip_was), (start_now, tip_now)] = BLADES;
    for i in 0..count {
        let segment = data + i * size;
        let (Some(start), Some(tip)) = (mem::read_finite::<3>(segment + start_now), mem::read_finite::<3>(segment + tip_now)) else { continue };
        let blade = [through(&back, start), through(&back, tip)];
        // The game's own segment list, on the game thread inside its own call.
        mem::write(segment + start_was, through(&before, blade[0]));
        mem::write(segment + tip_was, through(&before, blade[1]));
        mem::write(segment + start_now, through(&after, blade[0]));
        mem::write(segment + tip_now, through(&after, blade[1]));
    }
    REPLAY_SWEEPS.fetch_add(1, Relaxed);
    *replay = (to + 1 < trail.len()).then_some(trail[to].0);
}

/// End of a run: the swings replayed.
pub fn report_replays() {
    let replays = REPLAYS.load(Relaxed);
    if replays > 0 {
        log!("physical melee: {replays} swings replayed into the game's sweeps, over {} sweeps", REPLAY_SWEEPS.load(Relaxed));
    }
}

// ---- What the melee can hit ----

struct Search {
    active_level: ActiveLevelFn,
    find: FindInRadiusFn,
    extents: ExtentsFn,
    game: usize,
    target_class: usize,
}

static SEARCH: OnceLock<Search> = OnceLock::new();

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
/// Object classes (by vtable) the search leaves out (the player, the AI's target markers).
static LEFT_OUT: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

/// Game `G`'s search for what its melee can hit, readied (its `Hits::install_look`; the look runs
/// at the hand rig's posing moment).
pub fn install_look_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let targets = G::FACTS.melee_reach.ok_or_else(|| Lack::Missing("the melee's targets are not read for this build".into()))?;
    if !start.options.switch("melee_on_reach", true) {
        return Err(Lack::Missing("melee_on_reach is off".into()));
    }
    let gamedll = crate::feature::gamedll(start)?;
    install_search(G::engine(start.build), gamedll, targets).missing()
}

fn install_search(engine: &Module, gamedll: &Module, targets: Targets) -> Result<(), Rejection> {
    let find = |name: &str| engine.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let search = unsafe {
        Search {
            active_level: std::mem::transmute::<usize, ActiveLevelFn>(find(ACTIVE_LEVEL)?),
            find: std::mem::transmute::<usize, FindInRadiusFn>(find(FIND_IN_RADIUS)?),
            extents: std::mem::transmute::<usize, ExtentsFn>(find(EXTENTS)?),
            game: gamedll.at(targets.game),
            target_class: gamedll.at(targets.target_class),
        }
    };
    let _ = SEARCH.set(search);
    Ok(())
}

/// What the game's melee can hit within `radius` of `centre` (a game's `Hits::near`): each object
/// the level's search finds with the class the game's own combat target search passes, and its
/// box, the player and the AI's target markers left out.
pub fn near(centre: [f32; 3], radius: f32) -> Option<Vec<(usize, [f32; 6])>> {
    let search = SEARCH.get()?;
    let game = mem::read::<usize>(search.game).filter(|&g| g != 0)?;
    // SAFETY: the engine's own getter on the live game object, on the game's thread.
    let level = unsafe { (search.active_level)(game) };
    if level == 0 {
        return None;
    }
    let class = mem::read::<usize>(search.target_class).unwrap_or(0);
    let objects = {
        let mut found = FOUND.locked();
        found.ready();
        // SAFETY: the engine's search on its live level, on the game's thread; `found` is a vector
        // of its kind, set up as the game sets up its own; the filter is the class the game's own
        // search passes (or none, before the game set it).
        unsafe { (search.find)(level, &mut *found, &centre, radius, class, false, std::ptr::null_mut()) };
        found.items()
    };
    let near = objects
        .into_iter()
        .filter(|&object| !left_out(object))
        // SAFETY: the engine's getter on a live control object it just found.
        .filter_map(|object| mem::read::<[f32; 6]>(unsafe { (search.extents)(object) } as usize).map(|b| (object, b)))
        .collect();
    Some(near)
}

/// Whether `object` is the player or one of the AI's target markers, by its class (once per vtable).
fn left_out(object: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(object) else { return true };
    let mut known = LEFT_OUT.locked();
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

/// What a found object is, for the log (a game's `Hits::name`).
pub fn name(object: usize) -> String {
    class_name(object).unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests;
