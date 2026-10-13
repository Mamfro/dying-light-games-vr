//! The work that does not depend on the eye, done once per game frame: the right eye's scene call
//! skips those render-script commands (`engine::ONCE_COMMANDS`; their execute returns 1,
//! "continue"). Several classes share an execute, so each is hooked once and the skip is decided by
//! the command's own vtable.

use super::RIGHT_SCENE;
use crate::providers::engine::{self, CommandExecuteFn};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

const MAX: usize = engine::ONCE_COMMANDS.len();

static ORIGINALS: [Original<CommandExecuteFn>; MAX] = [const { Original::new() }; MAX];
const DETOURS: [CommandExecuteFn; MAX] =
    [once::<0>, once::<1>, once::<2>, once::<3>, once::<4>, once::<5>, once::<6>, once::<7>, once::<8>, once::<9>, once::<10>, once::<11>, once::<12>, once::<13>];
/// The renderer's base, and whether the right eye skips (`same_frame_skips`, on by default).
static RENDERER: AtomicUsize = AtomicUsize::new(0);
static SKIPPING: AtomicBool = AtomicBool::new(true);
static SKIPPED: AtomicU64 = AtomicU64::new(0);

/// Whether `command` is one of the commands the right eye skips.
fn skipped(command: usize) -> bool {
    let renderer = RENDERER.load(Relaxed);
    let Some(vtable) = mem::read::<usize>(command) else { return false };
    let vtable = vtable.wrapping_sub(renderer);
    engine::ONCE_COMMANDS.iter().any(|&(at, _)| at == vtable)
}

unsafe extern "system" fn once<const I: usize>(command: usize, context: usize) -> usize {
    let _flight = InFlight::enter();
    if RIGHT_SCENE.load(Acquire) && SKIPPING.load(Relaxed) && skipped(command) {
        SKIPPED.fetch_add(1, Relaxed);
        return 1;
    }
    // SAFETY: the original, with the renderer's arguments.
    unsafe { ORIGINALS[I].get()(command, context) }
}

/// Hooks each distinct execute of the commands (slot 1 of each vtable in `renderer`).
pub(super) fn install(hooks: &mut Hooks, renderer: &Module, skipping: bool) -> Result<(), Rejection> {
    RENDERER.store(renderer.base(), Relaxed);
    SKIPPING.store(skipping, Relaxed);
    let mut hooked: Vec<usize> = Vec::new();
    for &(vtable, name) in &engine::ONCE_COMMANDS {
        let Some(execute) = mem::read::<usize>(renderer.at(vtable) + 8).filter(|&e| renderer.contains(e)) else {
            log!("same-frame: {name}'s vtable holds no execute of the renderer; not skipped");
            continue;
        };
        if hooked.contains(&execute) {
            continue;
        }
        let i = hooked.len();
        // SAFETY: slot 1 of the command's vtable is its execute `(command, context) -> usize`; its
        // prologue is decoded and moved.
        unsafe { hooks.inline_decoded(&ORIGINALS[i], &format!("{name} execute"), execute, DETOURS[i])? };
        hooked.push(execute);
    }
    log!("same-frame: {} executes hooked for the right eye's skips ({})", hooked.len(), if skipping { "on" } else { "off: same_frame_skips=0" });
    Ok(())
}

pub(super) fn report() -> String {
    format!("{} eye-independent commands skipped by the right eye", SKIPPED.load(Relaxed))
}
