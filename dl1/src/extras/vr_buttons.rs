//! The VR button layout (`vr_buttons`, on by default), for controllers without a d-pad (Quest):
//!
//! - B turns the flashlight on and off (the d-pad up's action) instead of crouching;
//! - tapping Y heals (the d-pad down's action), holding Y still repairs; Y no longer looks back
//!   (in VR the head turns);
//! - ducking in the room crouches the character ([`crate::CROUCH_BY_DUCKING`], `monaka_arms::duck`),
//!   and with the hand rig and a gun drawn the left hand brought to the gun and taken away reloads
//!   ([`crate::RELOAD_BY_HAND`], `monaka_arms::reload`): both press the game's actions
//!   ([`crate::providers::actions`]), which this layout takes on buttons of its own.
//!
//! Crouching holds the game's hold-to-crouch action, bound to a button code no controller sends
//! ([`CROUCH_KEY`]); reloading also has such a code ([`RELOAD_KEY`]). They are pressed and let go
//! on the pad, through the input manager's pad button event ([`press`]), on the engine's input
//! pass, where the reload gesture is read too.
//!
//! The game builds its bindings from its input scripts (`inputs_pad.scr`) and registers them one by
//! one into its input manager ([`register`]); the layout is applied there, as the game's own
//! control presets do it, per action: B and Y keep their other uses (skipping a cutscene, leaving a
//! ladder, cancelling an arrow). The game builds them again once after the start and once at the
//! stop ([`dispatched`]), so nothing waits for a restart.
//!
//! The game has no tap-or-hold choice: on a button with a hold binding, the other bindings still
//! act as it goes down. So healing is not bound to Y itself: the pad's Y is watched at the input
//! manager's pad button event ([`pad_button`]), and a press let go before repairing starts
//! ([`TAP`]) presses the heal's own spare button code ([`HEAL_KEY`]) once, on the next input pass,
//! as a heal asked as an action does ([`crate::providers::actions`]).

use crate::providers::actions::{CROUCH_ASKED, take_reload};
use crate::providers::engine;
use crate::{ARMS, Dl1, flat_kept};
use monaka_arms::reload::{Reloading, Reloads};
use monaka_core::manifest::{Kind, Opt, Shown, opt};
use monaka_producer::game::{Component, Lack, On, Start, Switch};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};
use std::time::{Duration, Instant};

const VR_BUTTONS: Opt =
    opt("vr_buttons", "VR buttons", "B turns the flashlight on and off, tap Y to heal (hold Y still repairs), and duck in your room to crouch. Off: the game's own buttons.", Kind::Switch { default: true })
        .on(Shown::Game);

/// The VR button layout: B the flashlight, tap Y to heal, the hold-to-crouch and (with the hand rig
/// and `reload_gesture`) a reload on buttons of their own, for [`crate::CROUCH_BY_DUCKING`] and
/// [`crate::RELOAD_BY_HAND`].
pub const CONTROLS: Component<Dl1> = Component {
    name: "controls",
    options: &[VR_BUTTONS],
    switch: Some(Switch::new("vr_buttons", true, "the game's own buttons")),
    on: |_| On::when(flat_kept(), "flat: the game's own buttons"),
    install: install_controls,
    quiet: stop,
    ..Component::NONE
};

/// Reloading by hand reads the controllers' trail ([`hand_world::CONTROLLERS`]), each controller at
/// its latest place, at the layout's input pass ([`reload`]).
impl Reloads for Dl1 {
    const RELOADING: Reloading = Reloading::LATEST;
}

fn install_controls(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    let config = crate::config(start);
    let reload = config.reload_gesture && config.aim.rig && start.is_on(&ARMS);
    install(start.hooks, &start.build.gamedll, &start.build.engine, reload)?;
    Ok(())
}

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
/// The input manager's pad button event (`InputManager::OnInputJoyButton`, its vtable slot
/// +0x4a0): `(manager, pad, key, pressed)`. A press marks that pad as the controller last used;
/// the d-pad's keys go to the d-pad's state, the others run the pad bindings on that key for that
/// pad (the manager makes a copy of each pad binding per pad).
const PAD_BUTTON_SLOT: usize = 0x4a0;
const PAD_BUTTON: usize = 0x110fd80;
/// The pad last used (`-1` after the keyboard or mouse): the pad the buttons are pressed on.
const MANAGER_LAST_PAD: usize = 0x124;

/// A binding entry (0x2c bytes, one input action's definition): the action id, then the device at
/// +0x08 and the key at +0x10.
const ENTRY: usize = 0x2c;
const ENTRY_ACTION: usize = 0x00;
const ENTRY_KEY: usize = 0x10;

/// The pad's button codes (`data/inputenums.def`).
const KEY_B: u32 = 0x0f01;
const KEY_Y: u32 = 0x0f03;
const KEY_DPAD_UP: u32 = 0x0f0a;
const KEY_DPAD_DOWN: u32 = 0x0f0b;
/// `EJoy__BUTTON_15`: no pad sends it (the engine reads no SDL button that maps to it).
const HEAL_KEY: u32 = 0x0f0e;
/// `EJoy__BUTTON_16`: no pad sends it either.
const CROUCH_KEY: u32 = 0x0f0f;
/// A code past the pad's sixteen: only the reload gesture (and a reload asked) presses it.
const RELOAD_KEY: u32 = 0x0f10;
/// A Y let go sooner than this heals (repairing starts at 0.2 s held).
const TAP: Duration = Duration::from_millis(190);

/// The actions (their table entry, name and id, checked by name before the ids are trusted).
const DUCK_TOGGLE: (usize, &str, u32) = (0x1cde938, "_ACTION_DUCK_TOGGLE", 0x0e);
const DUCK: (usize, &str, u32) = (0x1cde950, "_ACTION_DUCK", 0x0f);
const RELOAD: (usize, &str, u32) = (0x1cde9e0, "_ACTION_RELOAD", 0x15);
const HEAL: (usize, &str, u32) = (0x1cded70, "_ACTION_HEAL", 0x47);
const FLASHLIGHT: (usize, &str, u32) = (0x1cdf028, "_ACTION_TOGGLE_FLASHLIGHT", 0x90);
const LOOKBACK: (usize, &str, u32) = (0x1cdf5c8, "_ACTION_LOOKBACK", 0xd5);
/// The buggy's heal (The Following), from a second action table: also on the heal's tap of Y
/// (holding Y there leaves the buggy). Its own check: on a mismatch only it is left out.
const VEHICLE_HEAL: (usize, &str, u32) = (0x1ce0000, "_ACTION_VEHICLE_HEAL", 0x144);
static VEHICLE_HEAL_KNOWN: AtomicBool = AtomicBool::new(false);
const KEY_X: u32 = 0x0f02;

type RegisterFn = unsafe extern "C" fn(*mut c_void, *mut u8);
type DispatchFn = unsafe extern "C" fn(*mut c_void, usize, *mut c_void);
type RebuildFn = unsafe extern "C" fn(usize);
type PadButtonFn = unsafe extern "C" fn(usize, i32, u32, u8);

static REGISTER_ORIGINAL: Original<RegisterFn> = Original::new();
static DISPATCH_ORIGINAL: Original<DispatchFn> = Original::new();
static PAD_BUTTON_ORIGINAL: Original<PadButtonFn> = Original::new();
/// The layout applies to bindings registered now (off for the rebuild at the stop).
pub(crate) static APPLYING: AtomicBool = AtomicBool::new(false);
/// A rebuild of the bindings is wanted, on the next input pass.
static REBUILD_WANTED: AtomicBool = AtomicBool::new(false);
static REBUILDS: AtomicU64 = AtomicU64::new(0);
static CHANGED: AtomicU64 = AtomicU64::new(0);
/// When the pad's Y went down (while held), and a tap of Y no input pass has taken yet.
static Y_DOWN: Mutex<Option<Instant>> = Mutex::new(None);
static Y_TAPPED: AtomicBool = AtomicBool::new(false);
/// When the heal button was pressed, while it is.
static HEAL_HELD: Mutex<Option<Instant>> = Mutex::new(None);
static HEALS: AtomicU64 = AtomicU64::new(0);
/// The crouch button as last pressed or released, and the crouches pressed.
static CROUCH_HELD: AtomicBool = AtomicBool::new(false);
static CROUCHES: AtomicU64 = AtomicU64::new(0);
/// The reload button bound (reloading by hand on), when it was pressed (while it is), the reloads.
pub(crate) static RELOAD_GESTURE: AtomicBool = AtomicBool::new(false);
static RELOAD_HELD: Mutex<Option<Instant>> = Mutex::new(None);
static RELOADS: AtomicU64 = AtomicU64::new(0);

fn actions_match(gamedll: &Module) -> Result<(), String> {
    for action in [DUCK_TOGGLE, DUCK, HEAL, FLASHLIGHT, LOOKBACK, RELOAD] {
        engine::action_matches(gamedll, action)?;
    }
    match engine::action_matches(gamedll, VEHICLE_HEAL) {
        Ok(()) => VEHICLE_HEAL_KNOWN.store(true, Relaxed),
        Err(why) => log!("VR buttons: the buggy's heal is left on the d-pad: {why}"),
    }
    Ok(())
}

pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module, reload_gesture: bool) -> Result<(), Rejection> {
    actions_match(gamedll).map_err(Rejection::revision)?;
    RELOAD_GESTURE.store(reload_gesture, Relaxed);
    // SAFETY: the detours have their function's signature (read from its code); the game DLL's
    // and the engine's builds are checked at start; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&REGISTER_ORIGINAL, "input binding registration", gamedll.at(REGISTER), register as RegisterFn)?;
        hooks.inline_decoded(&DISPATCH_ORIGINAL, "engine input pass", engine_module.at(DISPATCH), dispatched as DispatchFn)?;
        hooks.inline_decoded(&PAD_BUTTON_ORIGINAL, "pad button event", gamedll.at(PAD_BUTTON), pad_button as PadButtonFn)?;
    }
    APPLYING.store(true, Relaxed);
    REBUILD_WANTED.store(true, Relaxed);
    log!("VR buttons: B flashlight, tap Y heal (hold Y repair), crouch by ducking");
    Ok(())
}

/// At the stop, before the hooks come off (after ducking and reloading stopped asking): the game's
/// own bindings back, the crouch let go.
pub fn stop() {
    if REGISTER_ORIGINAL.is_set() {
        APPLYING.store(false, Relaxed);
        crate::providers::actions::forget();
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
            let_go_crouch(&gamedll, manager);
            // SAFETY: the game object's own rebuild, on the game's thread, between its input passes.
            unsafe { std::mem::transmute::<usize, RebuildFn>(gamedll.at(REBUILD))(game) };
            let n = REBUILDS.fetch_add(1, Relaxed) + 1;
            log!("VR buttons: the game's bindings built again ({n}){}", if APPLYING.load(Relaxed) { "" } else { ", as the game has them" });
        } else {
            log!("VR buttons: the game object's rebuild slot is {slot:#x?}, not the known function: bindings left as they are");
        }
        REBUILD_WANTED.store(false, Relaxed);
    }
    crouch(&gamedll, manager);
    // A heal is a press, let go [`PULSE`] later: the pass runs once per input device each update,
    // so a let-go on the next pass would come before the game looked at the button.
    if held_long_enough(&HEAL_HELD) {
        press(&gamedll, manager, HEAL_KEY, false);
    } else if APPLYING.load(Relaxed) && !is_held(&HEAL_HELD) && (Y_TAPPED.swap(false, Relaxed) || crate::providers::actions::take_heal()) && press(&gamedll, manager, HEAL_KEY, true) {
        hold(&HEAL_HELD);
        HEALS.fetch_add(1, Relaxed);
    }
    // A reload likewise.
    reload(&gamedll, manager);
}

/// Before the bindings are built again: the crouch button let go, if held.
fn let_go_crouch(gamedll: &Module, manager: usize) {
    if CROUCH_HELD.load(Relaxed) {
        press(gamedll, manager, CROUCH_KEY, false);
        CROUCH_HELD.store(false, Relaxed);
    }
}

/// On the engine's input pass: the crouch button pressed or let go as the crouch is asked
/// ([`crate::providers::actions`], by ducking).
fn crouch(gamedll: &Module, manager: usize) {
    let held = CROUCH_HELD.load(Relaxed);
    let wanted = crouch_wanted(CROUCH_ASKED.load(Relaxed), APPLYING.load(Relaxed), held, monaka_arms::traverse::traversing);
    if wanted != held && press(gamedll, manager, CROUCH_KEY, wanted) {
        CROUCH_HELD.store(wanted, Relaxed);
        if wanted {
            CROUCHES.fetch_add(1, Relaxed);
        }
    }
}

/// Whether the crouch button should be down: asked, with the layout applying. Hanging from a ledge
/// or climbing (`traversing`), a crouch not yet held does not start: the head going down then
/// crouches nothing.
fn crouch_wanted(asked: bool, applying: bool, held: bool, traversing: impl FnOnce() -> bool) -> bool {
    asked && applying && (held || !traversing())
}

/// On the engine's input pass: a reload pressed by the gesture or asked as an action
/// ([`crate::providers::actions`]), let go a pulse later, as a heal is.
fn reload(gamedll: &Module, manager: usize) {
    if held_long_enough(&RELOAD_HELD) {
        press(gamedll, manager, RELOAD_KEY, false);
    } else if APPLYING.load(Relaxed)
        && RELOAD_GESTURE.load(Relaxed)
        && !is_held(&RELOAD_HELD)
        && let Some(by) = reload_wanted(monaka_arms::reload::update::<Dl1>, take_reload)
        && press(gamedll, manager, RELOAD_KEY, true)
    {
        hold(&RELOAD_HELD);
        let n = RELOADS.fetch_add(1, Relaxed) + 1;
        if n <= 20 {
            log!("VR buttons: reload {n} by {by}");
        }
    }
}

/// Why a reload is wanted now: the gesture made (`gesture`, which asks for the reload action as it
/// presses it), else the action asked. The ask is taken (`take`) either way.
fn reload_wanted(gesture: impl FnOnce() -> bool, take: impl FnOnce() -> bool) -> Option<&'static str> {
    let gestured = gesture();
    take().then_some(if gestured { "the gesture" } else { "the reload action" })
}

/// The pad button event: a Y let go within [`TAP`] of going down is noted for the next input pass.
unsafe extern "C" fn pad_button(manager: usize, pad: i32, key: u32, pressed: u8) {
    let _flight = InFlight::enter();
    if key == KEY_Y
        && let Ok(mut down) = Y_DOWN.lock()
    {
        if pressed != 0 {
            *down = Some(Instant::now());
        } else if down.take().is_some_and(|at| at.elapsed() < TAP) {
            Y_TAPPED.store(true, Relaxed);
        }
    }
    // SAFETY: forwards the game's own call.
    unsafe { PAD_BUTTON_ORIGINAL.get()(manager, pad, key, pressed) };
}

/// How long a pressed button is held before it is let go.
const PULSE: Duration = Duration::from_millis(100);

fn hold(held: &Mutex<Option<Instant>>) {
    if let Ok(mut h) = held.lock() {
        *h = Some(Instant::now());
    }
}

fn is_held(held: &Mutex<Option<Instant>>) -> bool {
    held.lock().is_ok_and(|h| h.is_some())
}

/// Whether the button has been held [`PULSE`]: then it is taken as let go.
fn held_long_enough(held: &Mutex<Option<Instant>>) -> bool {
    let Ok(mut h) = held.lock() else { return false };
    if h.is_some_and(|at| at.elapsed() >= PULSE) {
        *h = None;
        return true;
    }
    false
}

/// Presses or lets go of `key` on the pad last used (the first pad when none was) through the
/// input manager's own pad button event; false when the manager's slot is not the known function.
fn press(gamedll: &Module, manager: usize, key: u32, down: bool) -> bool {
    let slot = mem::read::<usize>(manager).and_then(|vtable| mem::read::<usize>(vtable + PAD_BUTTON_SLOT));
    if slot != Some(gamedll.at(PAD_BUTTON)) {
        return false;
    }
    let pad = mem::read::<i32>(manager + MANAGER_LAST_PAD).filter(|&p| (0..16).contains(&p)).unwrap_or(0);
    // SAFETY: the input manager's own pad button event, on the game's thread, for a key only the
    // VR layout's bindings use.
    unsafe { PAD_BUTTON_ORIGINAL.get()(manager, pad, key, down as u8) };
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_arms::duck::{Ducking, RULES};
    use monaka_arms::melee::Held;
    use monaka_arms::reload::gestured;
    use monaka_core::gesture::{Duck, DuckRules, TouchAway};

    /// Dying Light 1's crouch before the ready-made: the head's crouch kept at each view, and on
    /// the input pass the button wanted down for it or for an asked crouch, with the layout
    /// applying; a crouch not yet held not started while traversing.
    struct OldCrouch {
        head: Duck,
        wanted: bool,
    }

    impl OldCrouch {
        fn note_head(&mut self, time: f64, y: f32) {
            if y.is_finite() {
                self.wanted = self.head.feed(time, y, &DuckRules::CROUCH);
            }
        }

        fn pass(&self, asked: bool, applying: bool, held: bool, traversing: bool) -> bool {
            let mut wanted = (self.wanted || asked) && applying;
            if wanted && !held && traversing {
                wanted = false;
            }
            wanted
        }
    }

    /// The head's height at view `i` (90 a second): standing with a little sway, ducks, a slow
    /// slouch, tracking lost now and then, a duck while climbing, and one held through a stop.
    fn head(i: usize) -> f32 {
        if i % 97 == 0 {
            return f32::NAN;
        }
        let sway = 0.01 * (i as f32 * 0.3).sin();
        let ducked = |from: usize, to: usize| {
            let depth = ((i.saturating_sub(from)).min(to.saturating_sub(i)) as f32 / 20.0).min(1.0);
            if (from..to).contains(&i) { 0.45 * depth } else { 0.0 }
        };
        let slouch = if (300..900).contains(&i) { 0.3 * (i - 300) as f32 / 600.0 } else { 0.0 };
        1.7 + sway - slouch - ducked(200, 300) - ducked(1000, 1100) - ducked(1510, 1650) - ducked(1900, 2400) - ducked(2600, 2700)
    }

    #[test]
    fn ducking_holds_the_crouch_at_the_passes_the_old_code_did() {
        let mut old = OldCrouch { head: Duck::new(), wanted: false };
        let mut ducking = Ducking::new();
        // The crouch as the actions provider hands it to Dying Light 1's own way (`CROUCH_ASKED`).
        let mut asked = false;
        let mut applying = true;
        ducking.start(|on| {
            asked = on;
            true
        });
        let (mut old_held, mut new_held) = (false, false);
        let (mut old_presses, mut new_presses) = (Vec::new(), Vec::new());
        for i in 1..3000 {
            let time = i as f64 / 90.0;
            old.note_head(time, head(i));
            ducking.feed(time, head(i), &RULES, |on| {
                asked = on;
                true
            });
            // The stop: ducking first, then the layout (its asks forgotten); the old code dropped
            // the head's crouch there. A start again while the head is down.
            if i == 2000 {
                ducking.stop(|on| {
                    asked = on;
                    true
                });
                old.wanted = false;
                applying = false;
                asked = false;
            }
            if i == 2301 {
                applying = true;
                ducking.start(|on| {
                    asked = on;
                    true
                });
            }
            // An input pass after every other view.
            if i % 2 == 1 {
                let traversing = (1500..1600).contains(&i);
                let old_wanted = old.pass(false, applying, old_held, traversing);
                let new_wanted = crouch_wanted(asked, applying, new_held, || traversing);
                assert_eq!(new_wanted, old_wanted, "the pass after view {i}");
                if old_wanted != old_held {
                    old_presses.push((i, old_wanted));
                    old_held = old_wanted;
                }
                if new_wanted != new_held {
                    new_presses.push((i, new_wanted));
                    new_held = new_wanted;
                }
            }
        }
        assert_eq!(new_presses, old_presses);
        // Each duck crouched and stood (the one climbing only once the climb was over), the stop
        // let go, and the start again crouched at once.
        assert_eq!(old_presses.len(), 12, "{old_presses:?}");
        assert!(old_presses.contains(&(1601, true)), "{old_presses:?}");
        assert!(old_presses.contains(&(2001, false)) && old_presses.contains(&(2301, true)), "{old_presses:?}");
    }

    #[test]
    fn the_crouch_button_follows_the_old_rule_in_every_case() {
        for bits in 0..32 {
            let [head, asked, applying, held, traversing] = [0, 1, 2, 3, 4].map(|b| bits & (1 << b) != 0);
            let old = OldCrouch { head: Duck::new(), wanted: head }.pass(asked, applying, held, traversing);
            assert_eq!(crouch_wanted(head || asked, applying, held, || traversing), old, "{bits:05b}");
        }
    }

    /// Dying Light 1's reload pass before the ready-made, with the button's pulse as passes
    /// (`pulse`): the gesture fed (a gun held, both controllers placed) only while the button is up,
    /// else an asked reload taken.
    struct OldReload {
        gesture: TouchAway,
        asked: bool,
        held_for: Option<usize>,
    }

    /// The new pass: the same gates, the ready-made's reading, and the reload it presses asked
    /// through Dying Light 1's own way and taken here.
    struct NewReload {
        gesture: TouchAway,
        asked: bool,
        held_for: Option<usize>,
    }

    const PULSE_PASSES: usize = 6;

    /// One pass's press: why, if a reload was pressed.
    fn old_pass(r: &mut OldReload, time: f64, applying: bool, gun: bool, apart: Option<f32>) -> Option<&'static str> {
        if let Some(n) = r.held_for.as_mut() {
            *n += 1;
            if *n >= PULSE_PASSES {
                r.held_for = None;
            }
            return None;
        }
        if !applying {
            return None;
        }
        let gestured = gun && apart.is_some_and(|apart| r.gesture.feed(time, apart, &Reloading::LATEST.touch));
        let by = if gestured { Some("the gesture") } else { std::mem::take(&mut r.asked).then_some("the reload action") };
        if by.is_some() {
            r.held_for = Some(0);
        }
        by
    }

    fn new_pass(r: &mut NewReload, time: f64, applying: bool, gun: bool, apart: Option<f32>) -> Option<&'static str> {
        if let Some(n) = r.held_for.as_mut() {
            *n += 1;
            if *n >= PULSE_PASSES {
                r.held_for = None;
            }
            return None;
        }
        if !applying {
            return None;
        }
        let held = if gun { Held::Gun } else { Held::Nothing };
        let asked = std::cell::Cell::new(r.asked);
        let gesture = &mut r.gesture;
        let by = reload_wanted(
            || {
                let done = gestured(gesture, time, held, apart, &Reloading::LATEST.touch);
                asked.set(asked.get() || done);
                done
            },
            || asked.replace(false),
        );
        r.asked = asked.get();
        if by.is_some() {
            r.held_for = Some(0);
        }
        by
    }

    #[test]
    fn the_reload_pass_presses_when_the_old_code_did() {
        let mut old = OldReload { gesture: TouchAway::new(), asked: false, held_for: None };
        let mut new = NewReload { gesture: TouchAway::new(), asked: false, held_for: None };
        let mut presses = Vec::new();
        for i in 0..4000 {
            let time = i as f64 / 120.0;
            // The left controller to the gun and away about each second, at varying speeds; lost
            // for a while; the gun put away for a while; the layout off for a while; a reload
            // asked as an action now and then.
            let phase = i % (100 + i / 400);
            let apart = match phase {
                _ if (700..760).contains(&i) => None,
                0..=29 => Some(0.4 - 0.011 * phase as f32),
                30..=44 => Some(0.07),
                _ => Some((0.07 + 0.004 * (phase - 44) as f32 * (1 + i % 3) as f32).min(0.5)),
            };
            let gun = !(1500..1700).contains(&i);
            let applying = !(2500..2600).contains(&i);
            if i % 700 == 350 {
                old.asked = true;
                new.asked = true;
            }
            let (was, now) = (old_pass(&mut old, time, applying, gun, apart), new_pass(&mut new, time, applying, gun, apart));
            assert_eq!(now, was, "pass {i}");
            if let Some(by) = now {
                presses.push((i, by));
            }
        }
        assert_eq!((old.gesture, old.asked), (new.gesture, new.asked));
        let by_gesture = presses.iter().filter(|(_, by)| *by == "the gesture").count();
        assert!(by_gesture >= 10 && presses.len() > by_gesture, "{presses:?}");
    }
}
