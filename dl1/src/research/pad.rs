//! `probe_pad`: what the engine asks SDL about game controllers (its imports of
//! `SDL_GameControllerGetButton` and `SDL_GameControllerGetAxis`): how often, for which
//! controllers, and the first buttons and axes that come back non-zero. Read-only.

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

type GetButtonFn = unsafe extern "C" fn(*mut c_void, i32) -> u8;
type GetAxisFn = unsafe extern "C" fn(*mut c_void, i32) -> i16;
type PollEventFn = unsafe extern "C" fn(*mut u8) -> i32;

static GET_BUTTON: Original<GetButtonFn> = Original::new();
static GET_AXIS: Original<GetAxisFn> = Original::new();
static POLL_EVENT: Original<PollEventFn> = Original::new();
static PAD_EVENTS: AtomicU64 = AtomicU64::new(0);
static BUTTON_CALLS: AtomicU64 = AtomicU64::new(0);
static AXIS_CALLS: AtomicU64 = AtomicU64::new(0);
static CONTROLLERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static LOGGED: AtomicU64 = AtomicU64::new(0);

pub fn install(hooks: &mut Hooks, engine: &Module) -> Result<(), Rejection> {
    // SAFETY: SDL's documented signatures (cdecl is the x64 convention).
    unsafe {
        hooks.import(&GET_BUTTON, "SDL_GameControllerGetButton", engine, "SDL2.dll", "SDL_GameControllerGetButton", get_button as GetButtonFn)?;
        hooks.import(&GET_AXIS, "SDL_GameControllerGetAxis", engine, "SDL2.dll", "SDL_GameControllerGetAxis", get_axis as GetAxisFn)?;
        hooks.import(&POLL_EVENT, "SDL_PollEvent", engine, "SDL2.dll", "SDL_PollEvent", poll_event as PollEventFn)?;
    }
    Ok(())
}

fn note_controller(controller: *mut c_void) {
    let Ok(mut known) = CONTROLLERS.lock() else { return };
    if !known.contains(&(controller as usize)) && known.len() < 8 {
        known.push(controller as usize);
        log!("pad probe: the engine reads SDL controller {controller:p}");
    }
}

unsafe extern "C" fn get_button(controller: *mut c_void, button: i32) -> u8 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the engine's own call.
    let pressed = unsafe { GET_BUTTON.get()(controller, button) };
    BUTTON_CALLS.fetch_add(1, Relaxed);
    note_controller(controller);
    if pressed != 0 && LOGGED.fetch_add(1, Relaxed) < 20 {
        log!("pad probe: SDL says button {button} pressed on {controller:p}");
    }
    pressed
}

unsafe extern "C" fn get_axis(controller: *mut c_void, axis: i32) -> i16 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the engine's own call.
    let value = unsafe { GET_AXIS.get()(controller, axis) };
    AXIS_CALLS.fetch_add(1, Relaxed);
    note_controller(controller);
    if value.unsigned_abs() > 8000 && LOGGED.fetch_add(1, Relaxed) < 20 {
        log!("pad probe: SDL says axis {axis} = {value} on {controller:p}");
    }
    value
}

/// SDL 2.0's joystick (0x600..) and game controller (0x650..) events the engine takes from its
/// event loop: type, the device's instance id, then the button or axis and its state or value.
unsafe extern "C" fn poll_event(event: *mut u8) -> i32 {
    let _flight = InFlight::enter();
    // SAFETY: forwards the engine's own call.
    let got = unsafe { POLL_EVENT.get()(event) };
    if got == 1 && !event.is_null() {
        // SAFETY: SDL filled the engine's SDL_Event (56 bytes).
        let bytes = unsafe { std::slice::from_raw_parts(event, 20) };
        let kind = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        // Keyboard: SDL_KEYDOWN/UP, state at 12, scancode at 16, key code at 20.
        if kind == 0x300 || kind == 0x301 {
            static KEYS: AtomicU64 = AtomicU64::new(0);
            if KEYS.fetch_add(1, Relaxed) < 40 {
                // SAFETY: SDL_KeyboardEvent is 32 bytes of the 56-byte SDL_Event.
                let key = unsafe { std::slice::from_raw_parts(event, 24) };
                let scancode = i32::from_le_bytes([key[16], key[17], key[18], key[19]]);
                let sym = i32::from_le_bytes([key[20], key[21], key[22], key[23]]);
                log!("pad probe: key {} scancode {scancode} key {sym:#x} ({:?})", if kind == 0x300 { "down" } else { "up" }, char::from_u32(sym as u32).filter(|c| c.is_ascii_graphic()));
            }
        }
        if (0x600..0x700).contains(&kind) {
            let n = PAD_EVENTS.fetch_add(1, Relaxed);
            let which = i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            // Axis events: axis at 12, value at 16; button events: button at 12, state at 13.
            let value = i16::from_le_bytes([bytes[16], bytes[17]]);
            if n < 60 {
                log!("pad probe: event {kind:#x} device {which} control {} state {} value {value}", bytes[12], bytes[13]);
            }
        }
    }
    got
}

pub fn report() {
    log!(
        "pad probe: the engine asked SDL for buttons {}x and axes {}x; it took {} joystick/controller events",
        BUTTON_CALLS.load(Relaxed),
        AXIS_CALLS.load(Relaxed),
        PAD_EVENTS.load(Relaxed)
    );
}
