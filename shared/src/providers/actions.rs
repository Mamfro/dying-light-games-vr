//! Provider part: actions (`monaka_pad::actions::Actions`), what the Chrome games share.
//!
//! The Chrome Engine games' actions on the pad, as far as all three games share them, for each
//! game's actions provider (`monaka_pad::actions::Actions`, implemented by each game on this): the
//! right trigger is the light attack (what a swing has always pressed). Dying Light 1 adds the
//! ones it knows ([`binding`] for the rest); Dying Light 2 and The Beast the ones their shared pad
//! layout has ([`binding_dl2`]: B toggles crouching, a tap of the d-pad's down the flashlight).
//!
//! Their VR button layout ([`VR_BUTTONS`], as Dying Light 1's): in play the controllers' B reaches
//! the game as the d-pad's down, so B is the flashlight, and crouching is by ducking, which taps the
//! game's B itself (`monaka_body::duck`). In a menu or a wheel B stays B ([`in_play`]).
//!
//! The reload, Dying Light 2's and The Beast's own way ([`engine`]), once the gun's update is hooked
//! ([`install_reload`]). The gun's controller (`WeaponFireController`) starts a reload from its
//! input when the reload action is pressed with the gun ready (state 0): it calls the reload start
//! with `from input` set (The Beast first marks some weapons, [`Prepare`]). The start checks for
//! itself that a reload can happen (spare ammo, the clip not full). After each update of the
//! player's ready gun, reloading by hand reads its gesture there (`monaka_body::reload::update`),
//! and a reload asked is started by that same call; the reload then runs as the game's.

use monaka_core::pad::{Gamepad, button};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_pad::actions::{Action, Ask, Binding, buttons};
use monaka_pad::remap::Remap;
use monaka_producer::{Rejection, log};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};

/// How every Chrome Engine game takes `action`; none where it is not known for all. The lockpick's
/// pick and turn are the game's own way once its minigame is hooked (`crate::providers::lockpick`);
/// never the sticks, which outside the minigame move the character.
pub fn binding(action: Action) -> Binding {
    match action {
        Action::Pick | Action::Turn if crate::providers::lockpick::installed() => Binding::Engine,
        _ => binding_with(action, HOOKED.load(Acquire)),
    }
}

/// The bindings, with the gun's update hooked or not (`hooked`).
fn binding_with(action: Action, hooked: bool) -> Binding {
    match action {
        Action::Attack => Binding::Pad(Gamepad { right_trigger: 255, ..Gamepad::default() }),
        Action::Reload if hooked => Binding::Engine,
        _ => Binding::None,
    }
}

/// How Dying Light 2 and The Beast take `action`: their pad's default layout, on top of what the
/// three games share ([`binding`]). B toggles crouching; a tap of the d-pad's down toggles the
/// flashlight (the game takes a press let go within 0.21 s, so the flashlight is pressed, not held).
pub fn binding_dl2(action: Action) -> Binding {
    match action {
        Action::Crouch => Binding::Toggle(buttons(button::B)),
        Action::Flashlight => Binding::Pad(buttons(button::DPAD_DOWN)),
        _ => binding(action),
    }
}

/// The VR button layout of Dying Light 2 and The Beast (a game's `Remaps`): the controllers' B
/// reaches the game as the d-pad's down, the flashlight, instead of the crouch toggle.
pub const VR_BUTTONS: &[Remap] = &[Remap { from: button::B, to: button::DPAD_DOWN }];

/// The d-pad directions that open the tools' (left) and the weapons' (right) wheels.
const WHEEL_BUTTONS: u16 = button::DPAD_LEFT | button::DPAD_RIGHT;
/// How old the pad's last read may be for its buttons to count (ms).
const READ_MAX_AGE_MS: u64 = 250;

/// Whether the VR button layout applies to a button going down now ([`VR_BUTTONS`]): in play. In a
/// menu (`crate::providers::gui::menu_open`), and while the world is not drawn or a video plays (a
/// cutscene's skip, a loading screen), B goes back or skips; with a wheel open or opening (the
/// d-pad's left or right held at the pad's last read) B picks a slot.
pub fn in_play() -> bool {
    use monaka_producer::situation::{Situation, active};
    let away = crate::providers::gui::menu_open() || active(Situation::WorldHidden) || active(Situation::Video);
    playing(away, monaka_pad::buttons(READ_MAX_AGE_MS))
}

/// [`in_play`] with a menu open or not (`menu`) and the buttons at the pad's last read.
fn playing(menu: bool, buttons: Option<u16>) -> bool {
    !menu && buttons.is_none_or(|b| b & WHEEL_BUTTONS == 0)
}

/// The Chrome games' own way (a game's `Actions::engine`): a reload asked is kept for the player's
/// gun's next update while it is ready; the pick and the turn followed are read by the minigame
/// itself (`crate::providers::lockpick`). False for anything else, or before the hook is in.
pub fn engine(action: Action, ask: Ask) -> bool {
    match (action, ask) {
        (Action::Reload, Ask::Tap { .. }) if HOOKED.load(Acquire) => {
            RELOAD_ASKED.store(true, Relaxed);
            true
        }
        (Action::Pick | Action::Turn, Ask::Follow | Ask::LetGo) => crate::providers::lockpick::installed(),
        _ => false,
    }
}

/// One build's gun controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Reload {
    /// The gun controller's update `(controller, dt)`.
    pub update: usize,
    /// The reload start `(controller, kind, from input)`.
    pub start: usize,
    /// The gun's state (i32): 0 ready.
    pub state: usize,
    /// What the input does before it starts a reload, where it does more than start it.
    pub prepare: Option<Prepare>,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// Before the start: the flag set when the gun's weapon (the controller's vtable slot) passes
/// either check `(weapon) -> bool`.
#[derive(Clone, Copy, Debug)]
pub struct Prepare {
    pub weapon_slot: usize,
    pub checks: [usize; 2],
    pub flag: usize,
}

/// The gun ready.
const READY: i32 = 0;

type UpdateFn = unsafe extern "C" fn(usize, f32);
type StartFn = unsafe extern "C" fn(usize, u8, u8);
type GetterFn = unsafe extern "C" fn(usize) -> usize;
type CheckFn = unsafe extern "C" fn(usize) -> bool;

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
/// The build, with the start, the player vtable and the prepare checks relocated.
static GUN: OnceLock<(Reload, usize, usize, Option<[usize; 2]>)> = OnceLock::new();
/// What runs at each update of the player's ready gun (reloading by hand's moment).
static MOMENT: OnceLock<fn() -> bool> = OnceLock::new();
/// The gun's update hooked; a reload asked; the reloads started.
static HOOKED: AtomicBool = AtomicBool::new(false);
static RELOAD_ASKED: AtomicBool = AtomicBool::new(false);
static RELOADS: AtomicU64 = AtomicU64::new(0);

/// Hooks `gun`'s update in the game DLL; `moment` runs after each update of the player's ready gun.
pub fn install_reload(hooks: &mut Hooks, gamedll: &Module, gun: Reload, moment: fn() -> bool) -> Result<(), Rejection> {
    let checks = gun.prepare.map(|p| p.checks.map(|c| gamedll.at(c)));
    let _ = GUN.set((gun, gamedll.at(gun.start), gamedll.at(gun.player_vtable), checks));
    let _ = MOMENT.set(moment);
    // SAFETY: the detour has the update's signature (a controller's update slot, as the engine
    // calls it); the game DLL's build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "gun update", gamedll.at(gun.update), update as UpdateFn)? };
    RELOAD_ASKED.store(false, Relaxed);
    HOOKED.store(true, Release);
    Ok(())
}

/// After an update of a gun (the player's: `players`) in `state`: whether a reload starts. Only
/// the player's ready gun gives the moment (`moment`, where a reload by hand is asked), so a
/// gesture made while the gun fires is not used up; then an asked reload is taken (`take`).
fn starts(players: bool, state: impl FnOnce() -> Option<i32>, moment: impl FnOnce() -> bool, take: impl FnOnce() -> bool) -> bool {
    if !players || state() != Some(READY) {
        return false;
    }
    moment();
    take()
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    let Some(&(gun, start, player_vtable, checks)) = GUN.get() else { return };
    let players = crate::providers::throws::players(controller, player_vtable);
    if !starts(players, || mem::read::<i32>(controller + gun.state), || MOMENT.get().is_some_and(|moment| moment()), || RELOAD_ASKED.swap(false, Relaxed)) {
        return;
    }
    if let (Some(prepare), Some(checks)) = (gun.prepare, checks) {
        let getter = mem::read::<usize>(controller).and_then(|vtable| mem::read::<usize>(vtable + prepare.weapon_slot)).filter(|&g| g != 0);
        // SAFETY: the controller's own weapon getter and the game's checks on that weapon, as its
        // input calls them on this thread before a reload.
        let marked = getter.is_some_and(|getter| unsafe {
            let weapon = std::mem::transmute::<usize, GetterFn>(getter)(controller);
            weapon != 0 && checks.iter().any(|&check| std::mem::transmute::<usize, CheckFn>(check)(weapon))
        });
        if marked {
            mem::write::<u8>(controller + prepare.flag, 1);
        }
    }
    RELOADS.fetch_add(1, Relaxed);
    // SAFETY: the game's own reload start on the player's ready gun, from its update on the game
    // thread (the update itself starts one there for an empty clip), as the reload input calls it.
    unsafe { std::mem::transmute::<usize, StartFn>(start)(controller, 0, 1) };
}

/// The reloads started, the hook out: the reload is not the game's own way any more.
pub fn report_reload() {
    if HOOKED.swap(false, AcqRel) {
        RELOAD_ASKED.store(false, Relaxed);
        log!("reload: {} reloads started on the player's gun", RELOADS.load(Relaxed));
    }
}

#[cfg(test)]
mod tests;
