//! Provider part: one game call aimed from the hands, the fire hook's means
//! ([`crate::providers::bow`], `monaka_body::launch::Fires`).
//!
//! The player's aim, from the hands for one game call: every controller's eye and look (and the
//! player's own shots) come from two functions of the player's aim part (player +0x1b0: its eye
//! point and its look direction, each `(part, out vec3) -> out`). While [`asking`] runs a call on a
//! thread, the first of the two asked there reads both as the game has them and asks `answer` with
//! them; its point and direction are then what both return, on that thread only, for the rest of
//! the call. So that one call (an arrow leaving the bow) aims from the hands while the camera and
//! everything else keep the head's aim. Without an answer the game's own stands.

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::Rejection;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// One build's aim part functions (game DLL RVAs).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The eye point `(part, out vec3) -> out` (vtable slot +0x58).
    pub eye: usize,
    /// The look direction `(part, out vec3) -> out` (vtable slot +0x50).
    pub look: usize,
}

type VectorFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *mut [f32; 3];

/// The answer for a call: from the game's eye point and look, the eye and look it aims with
/// instead (none: the game's).
pub type Answer = fn(eye: [f32; 3], look: [f32; 3]) -> Option<([f32; 3], [f32; 3])>;

/// The aim on a thread during [`asking`]: not asked yet, or the answer.
#[derive(Clone, Copy)]
enum Asking {
    Pending(Answer),
    Answered(Option<([f32; 3], [f32; 3])>),
}

static EYE_ORIGINAL: Original<VectorFn> = Original::new();
static LOOK_ORIGINAL: Original<VectorFn> = Original::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);
static OVERRIDDEN: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The call being aimed on this thread.
    static AIM: Cell<Option<Asking>> = const { Cell::new(None) };
}

/// Hooks `build`'s aim part functions in the game DLL (once, for every feature that aims a call).
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if INSTALLED.swap(true, Relaxed) {
        return Ok(());
    }
    // SAFETY: the detours have the targets' signatures (two vtable slots of one interface, as the
    // controllers call them); the game DLL's build is checked by the caller.
    unsafe {
        hooks.inline_decoded(&EYE_ORIGINAL, "aim part eye", gamedll.at(build.eye), eye as VectorFn)?;
        hooks.inline_decoded(&LOOK_ORIGINAL, "aim part look", gamedll.at(build.look), look as VectorFn)?;
    }
    Ok(())
}

/// Runs `call` with the player's aim on this thread as `answer` gives it, asked once with the
/// game's own eye point and look the first time either is read.
pub fn asking<R>(answer: Answer, call: impl FnOnce() -> R) -> R {
    let before = AIM.with(|a| a.replace(Some(Asking::Pending(answer))));
    let result = call();
    AIM.with(|a| a.set(before));
    result
}

/// The aim in force on this thread for the aim part `part`: asked now where it is pending.
fn aim(part: usize) -> Option<([f32; 3], [f32; 3])> {
    match AIM.with(Cell::get)? {
        Asking::Answered(aim) => aim,
        Asking::Pending(answer) => {
            let (mut eye, mut look) = ([0.0; 3], [0.0; 3]);
            // SAFETY: the aim part's own getters (the originals), on the thread that calls them,
            // each writing its out vector.
            unsafe {
                EYE_ORIGINAL.get()(part, &mut eye);
                LOOK_ORIGINAL.get()(part, &mut look);
            }
            let aim = answer(eye, look);
            AIM.with(|a| a.set(Some(Asking::Answered(aim))));
            aim
        }
    }
}

unsafe extern "C" fn eye(part: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let result = unsafe { EYE_ORIGINAL.get()(part, out) };
    if let Some((eye, _)) = aim(part) {
        // SAFETY: the caller's out vector, just written by the original.
        if let Some(point) = unsafe { result.as_mut() } {
            *point = eye;
            OVERRIDDEN.fetch_add(1, Relaxed);
        }
    }
    result
}

unsafe extern "C" fn look(part: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let result = unsafe { LOOK_ORIGINAL.get()(part, out) };
    if let Some((_, look)) = aim(part) {
        // SAFETY: the caller's out vector, just written by the original.
        if let Some(direction) = unsafe { result.as_mut() } {
            *direction = look;
        }
    }
    result
}

/// How many eye points were taken from the hands.
pub fn overridden() -> u64 {
    OVERRIDDEN.load(Relaxed)
}
