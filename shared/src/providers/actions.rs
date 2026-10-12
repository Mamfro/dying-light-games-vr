//! Provider part: actions (`monaka_pad::actions::Actions`), what the Chrome games share.
//!
//! The Chrome Engine games' actions on the pad, as far as all three games share them, for each
//! game's actions provider (`monaka_pad::actions::Actions`, implemented by each game on this): the
//! right trigger is the light attack (what a swing has always pressed). Dying Light 1 adds the
//! ones it knows ([`binding`] for the rest).
//!
//! The reload, Dying Light 2's and The Beast's own way ([`engine`]), once the gun's update is hooked
//! ([`install_reload`]). The gun's controller (`WeaponFireController`) starts a reload from its
//! input when the reload action is pressed with the gun ready (state 0): it calls the reload start
//! with `from input` set (The Beast first marks some weapons, [`Prepare`]). The start checks for
//! itself that a reload can happen (spare ammo, the clip not full). After each update of the
//! player's ready gun, reloading by hand reads its gesture there (`monaka_arms::reload::update`),
//! and a reload asked is started by that same call; the reload then runs as the game's.

use monaka_core::pad::Gamepad;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_pad::actions::{Action, Ask, Binding};
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
mod tests {
    use super::*;
    use monaka_arms::melee::Held;
    use monaka_arms::reload::{Reloading, gestured};
    use monaka_core::gesture::{TouchAway, TouchAwayRules};
    use monaka_pad::actions::standard;
    use std::cell::Cell;

    #[test]
    fn the_attack_is_the_trigger_a_swing_pressed() {
        assert_eq!(binding_with(Action::Attack, false), standard(Action::Attack));
        assert!(Action::ALL.iter().filter(|&&a| a != Action::Attack).all(|&a| binding_with(a, false) == Binding::None));
    }

    #[test]
    fn the_reload_is_the_games_own_once_the_gun_is_hooked() {
        assert_eq!(binding_with(Action::Reload, true), Binding::Engine);
        assert!(Action::ALL.iter().filter(|&&a| a != Action::Attack && a != Action::Reload).all(|&a| binding_with(a, true) == Binding::None));
        if !HOOKED.load(Acquire) {
            assert!(!engine(Action::Reload, Ask::Tap { ms: 100 }), "not hooked: refused");
        }
        assert!(!engine(Action::Attack, Ask::Tap { ms: 100 }));
    }

    /// The Chrome games' reload by hand before the ready-made, after each gun update: the
    /// player's ready gun, then the gesture read from both palms.
    fn old_update(gesture: &mut TouchAway, time: f64, players: bool, state: Option<i32>, apart: Option<f32>) -> bool {
        players && state == Some(READY) && apart.is_some_and(|apart| gesture.feed(time, apart, &TouchAwayRules::RELOAD))
    }

    #[test]
    fn a_reload_starts_at_the_gun_updates_the_old_code_started_one() {
        let (mut old, mut new) = (TouchAway::new(), TouchAway::new());
        let asked = Cell::new(false);
        let mut starts_seen = 0;
        for i in 0..3000 {
            let time = i as f64 / 60.0;
            // The left palm to the gun and away about each 1.3 s; the gun firing (not ready) now
            // and then, sometimes through a touch; another character's gun updating too; the
            // palms not fresh for a while.
            let phase = i % 80;
            let apart = match phase {
                _ if (1000..1050).contains(&i) => None,
                0..=19 => Some(0.4 - 0.015 * phase as f32),
                20..=29 => Some(0.08),
                _ => Some((0.08 + 0.02 * (phase - 29) as f32).min(0.5)),
            };
            let state = if (i / 37) % 5 == 4 { Some(2) } else { Some(READY) };
            for players in [false, true] {
                let was = old_update(&mut old, time, players, state, apart);
                // The ready-made at the moment: a gun in the aiming hand, the reload pressed
                // through the game's own way (kept as asked) and taken here.
                let now = starts(
                    players,
                    || state,
                    || {
                        let done = gestured(&mut new, time, Held::Gun, apart, &Reloading::FRESH.touch);
                        asked.set(asked.get() || done);
                        done
                    },
                    || asked.replace(false),
                );
                assert_eq!(now, was, "update {i}, the player's: {players}");
                starts_seen += now as usize;
            }
        }
        assert_eq!(old, new, "the gestures end in the same state");
        assert!(starts_seen >= 10, "{starts_seen}");
    }
}
