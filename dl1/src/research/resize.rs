//! `probe_resize`: who runs the levels' resolution-change handler, and when. The game DLL's
//! handler (`engine.rs`, the levels' UI after a size change) lays a level's menus out for a new
//! size; the game does not run it when the mod switches the size through the video settings, and
//! running it from the mod on the game's frame crashes the game. This logs each call the
//! game itself makes (the level's class, the thread, the code that called it), to find the point
//! the game runs it from (switching the display mode in the game's options, with `flat=1`).

use monaka_hook::module::Module;
use monaka_hook::probe::{callers, class_name};
use monaka_hook::{Hooks, InFlight, Instruction, Original};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// `gamedll+0x11442c0`, virtual 128 of the game DLL's levels and menu modules.
const HANDLER: usize = 0x11442c0;
const HANDLER_PROLOGUE: [Instruction; 1] = [Instruction::plain(&[0x48, 0x89, 0x6c, 0x24, 0x20])]; // mov [rsp+0x20], rbp
type HandlerFn = unsafe extern "system" fn(*mut core::ffi::c_void, usize, usize, usize) -> usize;

static ORIGINAL: Original<HandlerFn> = Original::new();
static CALLS: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn handler(level: *mut core::ffi::c_void, a: usize, b: usize, c: usize) -> usize {
    let _flight = InFlight::enter();
    let n = CALLS.fetch_add(1, Relaxed) + 1;
    if n <= 40 {
        let chain: Vec<String> = callers(10).into_iter().map(Module::describe).collect();
        log!(
            "probe_resize: call {n}: {} on thread {}; called from {}",
            class_name(level as usize).unwrap_or_else(|| "?".into()),
            monaka_hook::thread_id(),
            chain.join(" < ")
        );
    }
    // SAFETY: forwards the game's own call.
    unsafe { ORIGINAL.get()(level, a, b, c) }
}

/// Hooks the handler (its code checked against the expected prologue).
pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the target is checked against its exact prologue; the detour forwards the four
    // register arguments and the return value as they come.
    unsafe { hooks.inline(&ORIGINAL, "level resolution change", gamedll.at(HANDLER), &HANDLER_PROLOGUE, handler as HandlerFn) }?;
    log!("probe_resize: watching the levels' resolution-change handler");
    Ok(())
}

pub fn report() {
    log!("probe_resize: the game ran the handler {} times", CALLS.load(Relaxed));
}
