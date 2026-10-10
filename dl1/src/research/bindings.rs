//! `probe_bindings`: when the game builds its input bindings from the `inputs_*.scr` layouts, and
//! who asks. Each layout applies its chosen preset's operations to a list of bindings; this logs
//! every layout applied (its name, the preset, the list, how many bindings it held before and after,
//! the thread and the calling code) and, as each `AddAction` lands, the bindings on the buttons the
//! VR layout changes. Change the controller preset in the game's options to see whether the game
//! builds the list again.

use monaka_hook::module::Module;
use monaka_hook::probe::callers;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// `InputsLayout`'s apply (its vtable slot 2): `(layout, list, preset index)`.
const LAYOUT_APPLY: usize = 0x11f2520;
/// `InputsActionListOperation_AddAction`'s apply: `(operation, list)`, appending one binding.
const ADD_ACTION_APPLY: usize = 0x11eee30;
/// The layout's name (a `ttl::string`: the characters' pointer first).
const LAYOUT_NAME: usize = 0x08;
/// A list of bindings: `ttl::vector` of 0x2c-byte entries (pointer, then a u32 count).
const LIST_COUNT: usize = 0x08;
const ENTRY: usize = 0x2c;
/// In the operation: the binding it adds (the entry's bytes) from +0x08.
const OPERATION_ENTRY: usize = 0x08;
/// The pad buttons the VR layout changes: B, Y, the right stick click, d-pad up and down.
const WATCHED: [(u32, &str); 5] = [(0x0f01, "B"), (0x0f03, "Y"), (0x0f09, "right stick click"), (0x0f0a, "d-pad up"), (0x0f0b, "d-pad down")];

type LayoutApplyFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u32);
type AddActionApplyFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

static LAYOUT_APPLY_ORIGINAL: Original<LayoutApplyFn> = Original::new();
static ADD_ACTION_APPLY_ORIGINAL: Original<AddActionApplyFn> = Original::new();
static LAYOUTS: AtomicU64 = AtomicU64::new(0);
static ADDED: AtomicU64 = AtomicU64::new(0);

fn count(list: usize) -> u32 {
    mem::read::<u32>(list + LIST_COUNT).unwrap_or(0)
}

fn c_string(at: usize) -> String {
    let mut text = String::new();
    for i in 0..64 {
        match mem::read::<u8>(at + i) {
            Some(0) | None => break,
            Some(c) => text.push(c as char),
        }
    }
    text
}

unsafe extern "C" fn layout_apply(layout: *mut c_void, list: *mut c_void, preset: u32) {
    let _flight = InFlight::enter();
    let before = count(list as usize);
    // SAFETY: forwards the game's own call.
    unsafe { LAYOUT_APPLY_ORIGINAL.get()(layout, list, preset) };
    let n = LAYOUTS.fetch_add(1, Relaxed) + 1;
    if n <= 300 {
        let name = mem::read::<usize>(layout as usize + LAYOUT_NAME).map(c_string).unwrap_or_default();
        let chain: Vec<String> = callers(8).into_iter().map(Module::describe).collect();
        log!(
            "probe_bindings: layout {n} \"{name}\" preset {preset} into list {list:p}: {before} -> {} bindings, thread {}; called from {}",
            count(list as usize),
            monaka_hook::thread_id(),
            chain.join(" < ")
        );
    }
}

unsafe extern "C" fn add_action_apply(operation: *mut c_void, list: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { ADD_ACTION_APPLY_ORIGINAL.get()(operation, list) };
    let entry = operation as usize + OPERATION_ENTRY;
    let key = mem::read::<u32>(entry + 0x10).unwrap_or(0);
    if let Some((_, button)) = WATCHED.iter().find(|(k, _)| *k == key)
        && ADDED.fetch_add(1, Relaxed) < 400
    {
        let bytes = mem::read::<[u8; ENTRY]>(entry).unwrap_or([0; ENTRY]);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default());
        let float = |at: usize| f32::from_bits(word(at));
        log!(
            "probe_bindings: list {list:p} adds action {:#x} on {button}: target {} device {} flags {} {} {} silent {} hold {:.2} double {:.2} hysteresis {:.2}/{:.2} connected {:#x}",
            word(0),
            word(4),
            word(8),
            bytes[0x0c],
            bytes[0x14],
            bytes[0x15],
            bytes[0x16],
            float(0x20),
            float(0x24),
            float(0x18),
            float(0x1c),
            word(0x28),
        );
    }
}

pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: both detours have their function's signature (read from its code); the game DLL's
    // build is checked at start; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&LAYOUT_APPLY_ORIGINAL, "input layout apply", gamedll.at(LAYOUT_APPLY), layout_apply as LayoutApplyFn)?;
        hooks.inline_decoded(&ADD_ACTION_APPLY_ORIGINAL, "input AddAction apply", gamedll.at(ADD_ACTION_APPLY), add_action_apply as AddActionApplyFn)?;
    }
    log!("probe_bindings: watching the input layouts being applied");
    Ok(())
}

pub fn report() {
    log!("probe_bindings: {} layouts applied, {} watched bindings added", LAYOUTS.load(Relaxed), ADDED.load(Relaxed));
}
