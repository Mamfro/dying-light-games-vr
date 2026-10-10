//! Physical melee (`physical_melee`, with the hand rig) as Dying Light 2 and The Beast hit: a
//! swing hits where the weapon in the player's hand passes through an enemy, from the way it moved
//! (Dying Light 1's is its
//! own, `dl1/src/player/melee.rs`). The game's hit detection sweeps a blade from the arms'
//! `R_HandHolder` toward the weapon's `DTrailStart1`, frame to frame, against each enemy's bones,
//! and the hand rig poses that hand. Three things in it are made for the canned attack animation
//! instead, and are changed for the player's own controller:
//!
//! - the sweep runs only in a window that opens well after the attack starts (DL2: about 180 ms),
//!   by when a real swing is over: here it opens at the attack's start;
//! - the hit's direction is the attack animation's, through the controller's direction getter:
//!   here that getter returns zero, so each hit keeps the direction the hand moved;
//! - the blade is the weapon's range long (DL2: 1.49 m): here each swept segment is cut
//!   to the weapon in the hand ([`Physical::blade`]), or a fist's reach.

use monaka_arms::melee::{Held, Physical};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

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

/// The `IMeleeController` interface in the controller.
const INTERFACE: usize = 0x40;
/// The controller's player.
const PLAYER: usize = 0x08;
/// A segment's blades: the start's and the tip's offsets, previous and current.
const BLADES: [(usize, usize); 2] = [(0x00, 0x0c), (0x18, 0x24)];

type AttackStartFn = unsafe extern "C" fn(*mut c_void, i32);
type DirectionFn = unsafe extern "C" fn(*mut c_void) -> *const [f32; 3];
type TestSegmentsFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u8, *mut c_void, usize) -> usize;

static ATTACK_START_ORIGINAL: Original<AttackStartFn> = Original::new();
static DIRECTION_ORIGINAL: Original<DirectionFn> = Original::new();
static TEST_SEGMENTS_ORIGINAL: Original<TestSegmentsFn> = Original::new();

/// What the direction getter returns for the player: no override.
static NO_DIRECTION: [f32; 3] = [0.0; 3];

static BUILD: OnceLock<Build> = OnceLock::new();
static PHYSICAL: OnceLock<Physical> = OnceLock::new();
/// The player character's vtable, where the game DLL is.
static PLAYER_VTABLE: AtomicUsize = AtomicUsize::new(0);
static ATTACKS: AtomicU64 = AtomicU64::new(0);
static CUTS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s attack start, direction getter and segment test in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build, physical: Physical) -> Result<(), Rejection> {
    let _ = BUILD.set(build);
    let _ = PHYSICAL.set(physical);
    PLAYER_VTABLE.store(gamedll.at(build.player_vtable), Relaxed);
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&ATTACK_START_ORIGINAL, "melee attack start", gamedll.at(build.attack_start), attack_start as AttackStartFn)?;
        hooks.inline_decoded(&DIRECTION_ORIGINAL, "melee direction", gamedll.at(build.direction_getter), direction as DirectionFn)?;
        hooks.inline_decoded(&TEST_SEGMENTS_ORIGINAL, "melee segments", gamedll.at(build.test_segments), test_segments as TestSegmentsFn)?;
    }
    physical.apply();
    Ok(())
}

/// What the arms hold, if `player` is the player character and holds no gun.
fn player_melee(player: usize) -> Option<Held> {
    let held = monaka_arms::melee::held().filter(|&h| h != Held::Gun)?;
    let vtable = PLAYER_VTABLE.load(Relaxed);
    (vtable != 0 && mem::read::<usize>(player) == Some(vtable)).then_some(held)
}

/// The same for the controller `ctrl` (its player at +8).
fn controller_melee(ctrl: usize) -> Option<Held> {
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
    if !(window >= 0.0) {
        return;
    }
    // The game's own fields of its own controller, written on the game thread inside its call.
    mem::write::<f32>(ctrl + build.window_start, 0.0);
    mem::write::<f32>(ctrl + build.window_start_copy, 0.0);
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
    if let (Some(held), Some(physical), Some(build), Some((data, count))) = (player_melee(player as usize), PHYSICAL.get(), BUILD.get(), segments(list as usize)) {
        let length = physical.reach(held);
        for i in 0..count {
            let segment = data + i * build.segment;
            for (start, tip) in BLADES {
                let (Some(from), Some(to)) = (mem::read_finite::<3>(segment + start), mem::read_finite::<3>(segment + tip)) else { continue };
                let along = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
                let was = (along[0] * along[0] + along[1] * along[1] + along[2] * along[2]).sqrt();
                if was > 1e-4 {
                    let cut = along.map(|c| c / was * length);
                    // The game's own segment list, on the game thread inside its own call.
                    mem::write(segment + tip, [from[0] + cut[0], from[1] + cut[1], from[2] + cut[2]]);
                    let k = CUTS.fetch_add(1, Relaxed);
                    if k < 5 || k.is_power_of_two() {
                        log!("physical melee: blade {was:.2} m -> {length} m with {held:?} ({count} segments)");
                    }
                }
            }
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { TEST_SEGMENTS_ORIGINAL.get()(player, interface, list, hits, n) }
}
