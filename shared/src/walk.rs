//! The player's movement step as Dying Light 2 and The Beast take it, hooked for room-scale
//! following ([`monaka_arms::roomscale`]; Dying Light 1's walk body is its own,
//! `dl1/src/player/walk.rs`).
//!
//! The player is a Bullet kinematic character: each step the player's `PlayerBulletPhysicsModule`
//! (`module step(module, dt)`) sets a target position from its wanted velocity and calls the
//! engine's `CBulletPhysicsCharacter::GameStep(character, dt, cache, buffer)`, which sweeps the
//! capsule toward the target with collision. Following moves that target by its own velocity ×
//! dt, inside the player's step only, and measures how far the capsule got.

use monaka_arms::roomscale;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem, thread_id};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// One build's movement step and module fields.
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// `PlayerBulletPhysicsModule`'s step (game DLL RVA): `(module, dt in xmm1)`.
    pub module_step: usize,
    /// `CBulletPhysicsCharacter::GameStep` (engine RVA): `(character, dt in xmm1, sweep cache,
    /// history buffer)`.
    pub game_step: usize,
    /// Its first bytes, checked when the engine's build is not pinned.
    pub game_step_prologue: Option<&'static [u8]>,
    /// The player character's vtable (`PlayerDI_PH`, game DLL RVA).
    pub player_vtable: usize,
    /// An offset the module's step adds to the move each step (a 3x4 matrix; its translation at
    /// +0xc, +0x1c, +0x2c): animations that move the body themselves (searching a body, and the
    /// like) use it.
    pub step_offset: usize,
    /// Attached (to a vehicle, a rope...; 1): the step follows what it is attached to.
    pub attached: usize,
}

/// The module's player (`PlayerDI_PH`).
const MODULE_PLAYER: usize = 0x08;
/// A move-to target: when not zero, the step goes there instead of following the velocity.
const MOVE_TO: usize = 0x1c;
/// The character's target and current positions (world, metres).
const TARGET: usize = 0x128;
const POSITION: usize = 0x140;

/// Why the game moves the body itself this step (an index into [`REASONS`]; 0: it does not).
const REASONS: [Option<&str>; 5] = [None, Some("move-to target"), Some("attached"), Some("animated"), Some("no time")];

type ModuleStepFn = unsafe extern "C" fn(*mut c_void, f32) -> usize;
type GameStepFn = unsafe extern "C" fn(*mut c_void, f32, *mut c_void, *mut c_void) -> usize;

static MODULE_STEP_ORIGINAL: Original<ModuleStepFn> = Original::new();
static GAME_STEP_ORIGINAL: Original<GameStepFn> = Original::new();
static BUILD: OnceLock<Build> = OnceLock::new();
static PLAYER_VTABLE: AtomicUsize = AtomicUsize::new(0);
/// The thread running the player's module step (0 outside it): its `GameStep` is the player's
/// (other characters may step on other threads meanwhile). With why the game moves the body
/// itself this step, if it does.
static PLAYER_STEP_THREAD: AtomicU64 = AtomicU64::new(0);
static GAME_MOVES_IT: AtomicUsize = AtomicUsize::new(0);
static STEPS: AtomicU64 = AtomicU64::new(0);
static ANIMATED_STEPS: AtomicU64 = AtomicU64::new(0);

/// Hooks `build`'s module step (game DLL) and character sweep (engine).
pub fn install(hooks: &mut Hooks, engine: &Module, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if let Some(prologue) = build.game_step_prologue
        && !engine.bytes_match(build.game_step, prologue)
    {
        return Err(Rejection::revision("the engine's character sweep is not where it was inspected"));
    }
    let _ = BUILD.set(build);
    PLAYER_VTABLE.store(gamedll.at(build.player_vtable), Relaxed);
    // SAFETY: the detours have the targets' signatures (read from their code and call sites); the
    // engine's and the game DLL's builds are checked by the caller or above; prologues are
    // decoded and moved.
    unsafe {
        hooks.inline_decoded(&MODULE_STEP_ORIGINAL, "player physics step", gamedll.at(build.module_step), module_step as ModuleStepFn)?;
        hooks.inline_decoded(&GAME_STEP_ORIGINAL, "character sweep", engine.at(build.game_step), game_step as GameStepFn)?;
    }
    Ok(())
}

unsafe extern "C" fn module_step(module: *mut c_void, dt: f32) -> usize {
    let _flight = InFlight::enter();
    let at = module as usize;
    let vtable = PLAYER_VTABLE.load(Relaxed);
    let player = mem::read::<usize>(at + MODULE_PLAYER).and_then(mem::read::<usize>).is_some_and(|v| v == vtable && v != 0);
    let Some(build) = BUILD.get().filter(|_| player) else {
        // SAFETY: forwards the game's own call.
        return unsafe { MODULE_STEP_ORIGINAL.get()(module, dt) };
    };
    let move_to = mem::read::<[f32; 3]>(at + MOVE_TO).is_none_or(|t| t != [0.0; 3]);
    let attached = mem::read::<i32>(at + build.attached).is_none_or(|a| a == 1);
    let offset = [0x0c, 0x1c, 0x2c].map(|k| mem::read::<f32>(at + build.step_offset + k).unwrap_or(0.0));
    let animated = offset.iter().any(|c| c.abs() > 1e-5);
    if animated {
        ANIMATED_STEPS.fetch_add(1, Relaxed);
    }
    let reason = [move_to, attached, animated, dt <= 0.0].iter().position(|&r| r).map_or(0, |i| i + 1);
    GAME_MOVES_IT.store(reason, Relaxed);
    PLAYER_STEP_THREAD.store(thread_id() as u64, Relaxed);
    // SAFETY: forwards the game's own call.
    let result = unsafe { MODULE_STEP_ORIGINAL.get()(module, dt) };
    PLAYER_STEP_THREAD.store(0, Relaxed);
    result
}

unsafe extern "C" fn game_step(character: *mut c_void, dt: f32, cache: *mut c_void, history: *mut c_void) -> usize {
    let _flight = InFlight::enter();
    if PLAYER_STEP_THREAD.load(Relaxed) != thread_id() as u64 {
        // SAFETY: forwards the engine's own call.
        return unsafe { GAME_STEP_ORIGINAL.get()(character, dt, cache, history) };
    }
    let at = character as usize;
    let (from, target) = (mem::read_finite::<3>(at + POSITION), mem::read_finite::<3>(at + TARGET));
    // The game's own move this step, then following's on top of it.
    let game = match (from, target) {
        (Some(from), Some(target)) => [target[0] - from[0], target[1] - from[1], target[2] - from[2]],
        _ => [0.0; 3],
    };
    let follow = roomscale::follow(REASONS.get(GAME_MOVES_IT.load(Relaxed)).copied().flatten());
    if let Some(target) = target
        && follow != [0.0; 2]
        && dt > 0.0
    {
        mem::write(at + TARGET, [target[0] + follow[0] * dt, target[1], target[2] + follow[1] * dt]);
    }
    // SAFETY: forwards the engine's own call.
    let result = unsafe { GAME_STEP_ORIGINAL.get()(character, dt, cache, history) };
    if let (Some(from), Some(to)) = (from, mem::read_finite::<3>(at + POSITION)) {
        roomscale::credit([to[0] - from[0] - game[0], to[1] - from[1] - game[1], to[2] - from[2] - game[2]], follow, dt);
    }
    if STEPS.fetch_add(1, Relaxed) == 0 {
        log!("walk: the player's character steps (first dt {dt:.4} s)");
    }
    result
}

pub fn report() {
    let steps = STEPS.load(Relaxed);
    if steps > 0 {
        log!("walk: {steps} player character steps, {} moved by an animation's offset", ANIMATED_STEPS.load(Relaxed));
    }
}
