//! The VR button layout (`vr_buttons`, on by default), for controllers without a d-pad (Quest):
//!
//! - B turns the flashlight on and off (the d-pad up's action) instead of crouching;
//! - tapping Y heals (the d-pad down's action), holding Y still repairs; Y no longer looks back
//!   (in VR the head turns);
//! - ducking in the room crouches the character, and standing up stands it up again;
//! - with the hand rig and a gun drawn, the left hand brought to the gun and taken away reloads
//!   (`reload_gesture`, [`reload_gestured`]).
//!
//! The game builds its bindings from its input scripts (`inputs_pad.scr`) and registers them one by
//! one into its input manager ([`register`]); the layout is applied there, as the game's own
//! control presets do it, per action: B and Y keep their other uses (skipping a cutscene, leaving a
//! ladder, cancelling an arrow). The game builds them again once after the start and once at the
//! stop ([`dispatched`]), so nothing waits for a restart.
//!
//! Crouching holds the game's hold-to-crouch action, bound to a button code no controller sends
//! ([`CROUCH_KEY`]), pressed and released the way the game presses its own synthetic buttons (the
//! input manager's button event, slot +0x490), on the engine's input pass.
//!
//! The game has no tap-or-hold choice: on a button with a hold binding, the other bindings still
//! act as it goes down. So healing is not bound to Y itself: the pad's Y events are watched on the
//! engine's input pass, and a press let go before repairing starts ([`TAP`]) presses the heal's
//! own spare button code ([`HEAL_KEY`]) once.

use crate::engine;
use crate::player::hand_world;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

/// The input manager's registration of one binding: `(manager, binding entry)`; the entry is copied.
const REGISTER: usize = 0x110ea80;
/// The engine's input pass for a pad: `(input, pad index, events)`, on the game's thread; it hands
/// each event to the game's input manager.
const DISPATCH: usize = 0x2fb3c0;
/// The game object (`GameDI`) global; its input manager is at +0xc8 (busy while +0x128 is set).
const GAME: usize = 0x1c16348;
const MANAGER_IN_GAME: usize = 0xc8;
const MANAGER_BUSY: usize = 0x128;
/// `GameDI`'s rebuild of the input bindings (its vtable slot +0x8e0): clears the manager's
/// bindings and registers them again from the scripts.
const REBUILD_SLOT: usize = 0x8e0;
const REBUILD: usize = 0x454930;
/// The input manager's button event (its vtable slot +0x490): `(manager, key, pressed)`, for the
/// pad bindings (device 0) on that key.
const BUTTON_SLOT: usize = 0x490;
const BUTTON: usize = 0x110fa70;

/// A binding entry (0x2c bytes): the action id, then the device at +0x08 and the key at +0x10.
const ENTRY: usize = 0x2c;
const ENTRY_ACTION: usize = 0x00;
const ENTRY_KEY: usize = 0x10;

/// The pad's button codes (`data/inputenums.def`).
const KEY_B: u32 = 0x0f01;
const KEY_Y: u32 = 0x0f03;
const KEY_DPAD_UP: u32 = 0x0f0a;
const KEY_DPAD_DOWN: u32 = 0x0f0b;
/// `EJoy__BUTTON_15` and `_16`: no pad sends them (the engine reads no SDL button that maps to
/// them).
const HEAL_KEY: u32 = 0x0f0e;
const CROUCH_KEY: u32 = 0x0f0f;
/// In the engine's pad events (0x18 bytes: its button number, the event, ...): Y, and the button
/// events: 1 down; 2, 4 and 5 the others, which the game takes as up.
const EVENT: usize = 0x18;
const EVENT_KIND: usize = 0x04;
const ENGINE_Y: u32 = 0x14;
const EVENT_DOWN: u32 = 1;
const EVENT_UP: [u32; 3] = [2, 4, 5];
/// The input pass's devices (a list at +0x58); only pads' events are read.
const INPUT_DEVICES: usize = 0x58;
/// A Y let go sooner than this heals (repairing starts at 0.2 s held).
const TAP: Duration = Duration::from_millis(190);

/// The actions (their table entry, name and id, checked by name before the ids are trusted).
const DUCK_TOGGLE: (usize, &str, u32) = (0x1cde938, "_ACTION_DUCK_TOGGLE", 0x0e);
const DUCK: (usize, &str, u32) = (0x1cde950, "_ACTION_DUCK", 0x0f);
const HEAL: (usize, &str, u32) = (0x1cded70, "_ACTION_HEAL", 0x47);
const FLASHLIGHT: (usize, &str, u32) = (0x1cdf028, "_ACTION_TOGGLE_FLASHLIGHT", 0x90);
const LOOKBACK: (usize, &str, u32) = (0x1cdf5c8, "_ACTION_LOOKBACK", 0xd5);
const RELOAD: (usize, &str, u32) = (0x1cde9e0, "_ACTION_RELOAD", 0x15);
/// The buggy's heal (The Following), from a second action table: also on the heal's tap of Y
/// (holding Y there leaves the buggy). Its own check: on a mismatch only it is left out.
const VEHICLE_HEAL: (usize, &str, u32) = (0x1ce0000, "_ACTION_VEHICLE_HEAL", 0x144);
static VEHICLE_HEAL_KNOWN: AtomicBool = AtomicBool::new(false);
const KEY_X: u32 = 0x0f02;
/// A code past the pad's sixteen: only the reload gesture presses it.
const RELOAD_KEY: u32 = 0x0f10;

/// The reload gesture (`reload_gesture`, with the hand rig and a gun drawn): the left hand brought
/// within [`RELOAD_NEAR`] of the gun hand, then taken more than [`RELOAD_AWAY`] away within
/// [`RELOAD_WITHIN`], reloads; not again for [`RELOAD_COOLDOWN`].
const RELOAD_NEAR: f32 = 0.12;
const RELOAD_AWAY: f32 = 0.25;
const RELOAD_WITHIN: Duration = Duration::from_millis(1000);
const RELOAD_COOLDOWN: Duration = Duration::from_millis(1500);

/// The head this far (metres) below standing crouches; back within [`STAND_DROP`] stands.
const CROUCH_DROP: f32 = 0.35;
const STAND_DROP: f32 = 0.22;
/// While standing, the standing height follows a lower head down this fast (m/s): sitting down
/// or the tracking drifting does not leave the player crouched.
const STANDING_SINK: f32 = 0.02;

type RegisterFn = unsafe extern "C" fn(*mut c_void, *mut u8);
type DispatchFn = unsafe extern "C" fn(*mut c_void, usize, *mut c_void);
type RebuildFn = unsafe extern "C" fn(usize);
type ButtonFn = unsafe extern "C" fn(usize, u32, u32);

static REGISTER_ORIGINAL: Original<RegisterFn> = Original::new();
static DISPATCH_ORIGINAL: Original<DispatchFn> = Original::new();
/// The layout applies to bindings registered now (off for the rebuild at the stop).
static APPLYING: AtomicBool = AtomicBool::new(false);
/// A rebuild of the bindings is wanted, on the next input pass.
static REBUILD_WANTED: AtomicBool = AtomicBool::new(false);
static REBUILDS: AtomicU64 = AtomicU64::new(0);
static CHANGED: AtomicU64 = AtomicU64::new(0);
/// The crouch button as last pressed or released, and whether it should be held now.
static CROUCH_HELD: AtomicBool = AtomicBool::new(false);
static CROUCH_WANTED: AtomicBool = AtomicBool::new(false);
static CROUCHES: AtomicU64 = AtomicU64::new(0);
/// When the pad's Y went down (while held), whether the heal button is down, and heals pressed.
static Y_DOWN: Mutex<Option<Instant>> = Mutex::new(None);
static HEAL_HELD: AtomicBool = AtomicBool::new(false);
static HEALS: AtomicU64 = AtomicU64::new(0);
/// The reload gesture: on, its button down, when the left hand last touched the gun, the last reload.
static RELOAD_GESTURE: AtomicBool = AtomicBool::new(false);
static RELOAD_HELD: AtomicBool = AtomicBool::new(false);
static GESTURE: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));
static RELOADS: AtomicU64 = AtomicU64::new(0);
/// The input devices seen, with whether each is a pad (by its class).
static PADS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

/// The standing head height (tracking space) and when the head was last seen.
struct Head {
    standing: f32,
    seen: Instant,
}
static HEAD: Mutex<Option<Head>> = Mutex::new(None);

fn action_matches(gamedll: &Module, (entry, name, id): (usize, &str, u32)) -> Result<(), String> {
    let at = gamedll.at(entry);
    let found = mem::read::<usize>(at).and_then(|text| mem::read_c_string(text, 64));
    let found_id = mem::read::<u32>(at + 0x10);
    if found.as_deref() != Some(name) || found_id != Some(id) {
        return Err(format!("action table entry {entry:#x} is {found:?} id {found_id:?}, not {name} id {id}"));
    }
    Ok(())
}

fn actions_match(gamedll: &Module) -> Result<(), String> {
    for action in [DUCK_TOGGLE, DUCK, HEAL, FLASHLIGHT, LOOKBACK, RELOAD] {
        action_matches(gamedll, action)?;
    }
    match action_matches(gamedll, VEHICLE_HEAL) {
        Ok(()) => VEHICLE_HEAL_KNOWN.store(true, Relaxed),
        Err(why) => log!("VR buttons: the buggy's heal is left on the d-pad: {why}"),
    }
    Ok(())
}

pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module, reload_gesture: bool) -> Result<(), Rejection> {
    actions_match(gamedll).map_err(Rejection::revision)?;
    RELOAD_GESTURE.store(reload_gesture, Relaxed);
    // SAFETY: both detours have their function's signature (read from its code); the game DLL's
    // and the engine's builds are checked at start; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&REGISTER_ORIGINAL, "input binding registration", gamedll.at(REGISTER), register as RegisterFn)?;
        hooks.inline_decoded(&DISPATCH_ORIGINAL, "engine input pass", engine_module.at(DISPATCH), dispatched as DispatchFn)?;
    }
    APPLYING.store(true, Relaxed);
    REBUILD_WANTED.store(true, Relaxed);
    log!("VR buttons: B flashlight, tap Y heal (hold Y repair), crouch by ducking");
    Ok(())
}

/// At the stop, before the hooks come off: the game's own bindings back, the crouch let go.
pub fn stop() {
    if REGISTER_ORIGINAL.is_set() {
        APPLYING.store(false, Relaxed);
        CROUCH_WANTED.store(false, Relaxed);
        REBUILD_WANTED.store(true, Relaxed);
        let started = Instant::now();
        while (REBUILD_WANTED.load(Relaxed) || CROUCH_HELD.load(Relaxed)) && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
        log!(
            "VR buttons: {} bindings changed over {} rebuilds, {} crouches, {} heals by a tap of Y, {} reloads by the gesture{}",
            CHANGED.load(Relaxed),
            REBUILDS.load(Relaxed),
            CROUCHES.load(Relaxed),
            HEALS.load(Relaxed),
            RELOADS.load(Relaxed),
            if REBUILD_WANTED.load(Relaxed) { "; the game's own bindings come back when it next builds them (it ran no input pass)" } else { "" }
        );
    }
}

/// The head's height in tracking space (y up), each view.
pub fn note_head(y: f32) {
    if !y.is_finite() {
        return;
    }
    let Ok(mut head) = HEAD.lock() else { return };
    let now = Instant::now();
    let crouched = CROUCH_WANTED.load(Relaxed);
    let state = head.get_or_insert(Head { standing: y, seen: now });
    let dt = now.duration_since(state.seen).as_secs_f32().min(0.1);
    state.seen = now;
    if y > state.standing {
        state.standing = y;
    } else if !crouched {
        state.standing = (state.standing - STANDING_SINK * dt).max(y);
    }
    let drop = state.standing - y;
    let wanted = if crouched { drop > STAND_DROP } else { drop > CROUCH_DROP };
    CROUCH_WANTED.store(wanted, Relaxed);
}

fn copy_with(entry: *const u8, action: u32, key: u32) -> [u8; ENTRY] {
    let mut copy = mem::read::<[u8; ENTRY]>(entry as usize).unwrap_or([0; ENTRY]);
    copy[ENTRY_ACTION..ENTRY_ACTION + 4].copy_from_slice(&action.to_le_bytes());
    copy[ENTRY_KEY..ENTRY_KEY + 4].copy_from_slice(&key.to_le_bytes());
    copy
}

/// Each binding the game registers: the VR layout applied to it.
unsafe extern "C" fn register(manager: *mut c_void, entry: *mut u8) {
    let _flight = InFlight::enter();
    // SAFETY: the game's own call, forwarded or made again with a copy of its entry.
    let forward = |entry: *mut u8| unsafe { REGISTER_ORIGINAL.get()(manager, entry) };
    if !APPLYING.load(Relaxed) || entry.is_null() {
        return forward(entry);
    }
    let action = mem::read::<u32>(entry as usize + ENTRY_ACTION).unwrap_or(u32::MAX);
    let key = mem::read::<u32>(entry as usize + ENTRY_KEY).unwrap_or(u32::MAX);
    match (action, key) {
        // Crouching moves off B, to the button the head presses.
        (a, KEY_B) if a == DUCK_TOGGLE.2 => {
            CHANGED.fetch_add(1, Relaxed);
            forward(copy_with(entry, DUCK.2, CROUCH_KEY).as_mut_ptr());
        }
        // Looking back is turning the head.
        (a, KEY_Y) if a == LOOKBACK.2 => {
            CHANGED.fetch_add(1, Relaxed);
        }
        // The d-pad's flashlight and heal, also on B and Y.
        (a, KEY_DPAD_UP) if a == FLASHLIGHT.2 => {
            CHANGED.fetch_add(1, Relaxed);
            forward(entry);
            forward(copy_with(entry, action, KEY_B).as_mut_ptr());
        }
        (a, KEY_DPAD_DOWN) if a == HEAL.2 || (a == VEHICLE_HEAL.2 && VEHICLE_HEAL_KNOWN.load(Relaxed)) => {
            CHANGED.fetch_add(1, Relaxed);
            forward(entry);
            forward(copy_with(entry, action, HEAL_KEY).as_mut_ptr());
        }
        // Reloading, also on the gesture's button.
        (a, KEY_X) if a == RELOAD.2 && RELOAD_GESTURE.load(Relaxed) => {
            CHANGED.fetch_add(1, Relaxed);
            forward(entry);
            forward(copy_with(entry, action, RELOAD_KEY).as_mut_ptr());
        }
        _ => forward(entry),
    }
}

/// The engine's input pass, on the game's thread: after it, a wanted rebuild and the crouch button.
unsafe extern "C" fn dispatched(input: *mut c_void, pad: usize, events: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the engine's own call.
    unsafe { DISPATCH_ORIGINAL.get()(input, pad, events) };
    let tapped = is_pad(input as usize, pad) && y_tapped(events as usize);
    let Some(gamedll) = Module::find(engine::GAMEDLL) else { return };
    let Some(game) = mem::read::<usize>(gamedll.at(GAME)).filter(|&g| g != 0) else { return };
    let Some(manager) = mem::read::<usize>(game + MANAGER_IN_GAME).filter(|&m| m != 0) else { return };
    if mem::read::<u8>(manager + MANAGER_BUSY) != Some(0) {
        return;
    }
    if REBUILD_WANTED.load(Relaxed) {
        let slot = mem::read::<usize>(game).and_then(|vtable| mem::read::<usize>(vtable + REBUILD_SLOT));
        if slot == Some(gamedll.at(REBUILD)) {
            // The crouch button is let go first: its binding is about to be replaced.
            if CROUCH_HELD.load(Relaxed) {
                press(&gamedll, manager, CROUCH_KEY, false);
                CROUCH_HELD.store(false, Relaxed);
            }
            // SAFETY: the game object's own rebuild, on the game's thread, between its input passes.
            unsafe { std::mem::transmute::<usize, RebuildFn>(gamedll.at(REBUILD))(game) };
            let n = REBUILDS.fetch_add(1, Relaxed) + 1;
            log!("VR buttons: the game's bindings built again ({n}){}", if APPLYING.load(Relaxed) { "" } else { ", as the game has them" });
        } else {
            log!("VR buttons: the game object's rebuild slot is {slot:#x?}, not the known function: bindings left as they are");
        }
        REBUILD_WANTED.store(false, Relaxed);
    }
    let mut wanted = CROUCH_WANTED.load(Relaxed) && APPLYING.load(Relaxed);
    // Hanging from a ledge or climbing, crouching lets go: the head going down then crouches nothing.
    if wanted && !CROUCH_HELD.load(Relaxed) && monaka_arms::traverse::traversing() {
        wanted = false;
    }
    if wanted != CROUCH_HELD.load(Relaxed) && press(&gamedll, manager, CROUCH_KEY, wanted) {
        CROUCH_HELD.store(wanted, Relaxed);
        if wanted {
            CROUCHES.fetch_add(1, Relaxed);
        }
    }
    // A heal is a press, then a let-go on a later pass.
    if HEAL_HELD.load(Relaxed) {
        press(&gamedll, manager, HEAL_KEY, false);
        HEAL_HELD.store(false, Relaxed);
    } else if tapped && APPLYING.load(Relaxed) && press(&gamedll, manager, HEAL_KEY, true) {
        HEAL_HELD.store(true, Relaxed);
        HEALS.fetch_add(1, Relaxed);
    }
    // A reload is a press, then a let-go on a later pass.
    if RELOAD_HELD.load(Relaxed) {
        press(&gamedll, manager, RELOAD_KEY, false);
        RELOAD_HELD.store(false, Relaxed);
    } else if APPLYING.load(Relaxed) && RELOAD_GESTURE.load(Relaxed) && reload_gestured() && press(&gamedll, manager, RELOAD_KEY, true) {
        RELOAD_HELD.store(true, Relaxed);
        let n = RELOADS.fetch_add(1, Relaxed) + 1;
        if n <= 20 {
            log!("VR buttons: reload {n} by the gesture");
        }
    }
}

/// With a gun drawn: the left hand came to the gun hand and has just been taken away from it.
fn reload_gestured() -> bool {
    if monaka_arms::melee::held() != Some(monaka_arms::melee::Held::Gun) {
        return false;
    }
    let (Some(left), Some(right)) = (hand_world::position(hand_world::LEFT), hand_world::position(hand_world::RIGHT)) else { return false };
    let apart = hand_world::length([0, 1, 2].map(|k| left[k] - right[k]));
    let Ok(mut gesture) = GESTURE.lock() else { return false };
    let now = Instant::now();
    let (touched, last) = &mut *gesture;
    if apart < RELOAD_NEAR {
        *touched = Some(now);
        return false;
    }
    let fresh = touched.is_some_and(|at| now.duration_since(at) < RELOAD_WITHIN);
    let rested = last.is_none_or(|at| now.duration_since(at) > RELOAD_COOLDOWN);
    if apart > RELOAD_AWAY && fresh && rested {
        *touched = None;
        *last = Some(now);
        return true;
    }
    false
}

/// Whether the input pass's device `index` is a pad (its class, looked up once per device).
fn is_pad(input: usize, index: usize) -> bool {
    let device = mem::read::<usize>(input + INPUT_DEVICES).and_then(|list| mem::read::<usize>(list + index * 8));
    let Some(device) = device.filter(|&d| d != 0) else { return false };
    let Ok(mut known) = PADS.lock() else { return false };
    if let Some(&(_, pad)) = known.iter().find(|(d, _)| *d == device) {
        return pad;
    }
    let class = monaka_hook::probe::class_name(device).unwrap_or_default();
    let pad = class.contains("PadDevice");
    log!("VR buttons: input device {index} is {class} ({})", if pad { "a pad: its Y is watched" } else { "not a pad" });
    if known.len() < 64 {
        known.push((device, pad));
    }
    pad
}

/// Reads a pad's events for Y: whether Y was let go this pass within [`TAP`] of going down.
fn y_tapped(events: usize) -> bool {
    let (Some(list), Some(count)) = (mem::read::<usize>(events), mem::read::<u32>(events + 8)) else { return false };
    let Ok(mut down) = Y_DOWN.lock() else { return false };
    let mut tapped = false;
    for i in 0..count.min(64) as usize {
        let at = list + i * EVENT;
        if mem::read::<u32>(at) != Some(ENGINE_Y) {
            continue;
        }
        match mem::read::<u32>(at + EVENT_KIND) {
            Some(EVENT_DOWN) => *down = Some(Instant::now()),
            Some(kind) if EVENT_UP.contains(&kind) => tapped |= down.take().is_some_and(|at| at.elapsed() < TAP),
            _ => {}
        }
    }
    tapped
}

/// Presses or lets go of `key` through the input manager's own button event; false when the
/// manager's slot is not the known function.
fn press(gamedll: &Module, manager: usize, key: u32, down: bool) -> bool {
    let slot = mem::read::<usize>(manager).and_then(|vtable| mem::read::<usize>(vtable + BUTTON_SLOT));
    if slot != Some(gamedll.at(BUTTON)) {
        return false;
    }
    // SAFETY: the input manager's own button event, on the game's thread, for a key only the VR
    // layout's bindings use.
    unsafe { std::mem::transmute::<usize, ButtonFn>(gamedll.at(BUTTON))(manager, key, down as u32) };
    true
}
