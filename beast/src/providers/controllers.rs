//! Providers: a situations detector (whether the player climbs, from the game itself) for the hand
//! rig (`monaka_body::traverse::report_climbing`, reported at the arms callback's moment:
//! [`crate::providers::hand_rig::moment`]), and the player each frame, where the arms' anti-wall
//! scale is held ([`crate::providers::anti_wall`]).
//!
//! The player's activities run as controllers in groups (`controllers_setup.scr`), and only the
//! controllers of the groups the player is in are updated; every player controller has its player
//! at +8. A ladder, a ledge (edge, hang, climb, shimmy), a zip line and a free climb each have their
//! own controllers, in their own group and no other: one of them updating is the player climbing.
//! Each one's update is hooked and notes when it ran ([`climbing`]): the arms go to the game's
//! animation exactly then, not whenever the animation raises the hands (crawling, a throw's
//! wind-up), as Dying Light 1's report. The look controller is in almost every group: its update
//! hands over the player every frame.

use crate::Beast;
use crate::providers::engine::{CLIMBING_UPDATES, LOOK_UPDATE, PLAYER_VTABLE};
use eng_chr::providers::throws::players;
use monaka_hook::{InFlight, Original};
use monaka_producer::game::{Lack, Missing, Start};
use monaka_producer::log;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

/// An update of a climbing controller this recent is the player climbing (ms): the player's
/// controllers update every frame.
const FRESH_MS: u64 = 150;

type UpdateFn = unsafe extern "C" fn(usize, f32);

const COUNT: usize = CLIMBING_UPDATES.len();
static ORIGINALS: [Original<UpdateFn>; COUNT] = [const { Original::new() }; COUNT];
const DETOURS: [UpdateFn; COUNT] = [update::<0>, update::<1>, update::<2>, update::<3>, update::<4>, update::<5>, update::<6>];
static LOOK_ORIGINAL: Original<UpdateFn> = Original::new();
/// The player's vtable, relocated (0 until the hooks are in).
static VTABLE: AtomicUsize = AtomicUsize::new(0);
/// When one of the player's climbing controllers last updated (tick), and which (its index).
static LAST: AtomicU64 = AtomicU64::new(0);
static WHICH: AtomicUsize = AtomicUsize::new(0);

/// Hooks the climbing controllers' and the look controller's updates in the game DLL (once).
pub fn install(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    if VTABLE.load(Relaxed) != 0 {
        return Ok(());
    }
    let gamedll = eng_chr::feature::gamedll(start)?;
    // SAFETY (each hook): the detours have the updates' signature (every controller's update: the
    // controller and the frame's time); the game DLL is the build the facts describe; the
    // prologues are decoded and moved.
    for (i, &(name, rva)) in CLIMBING_UPDATES.iter().enumerate() {
        unsafe { start.hooks.inline_decoded(&ORIGINALS[i], name, gamedll.at(rva), DETOURS[i]) }.missing()?;
    }
    unsafe { start.hooks.inline_decoded(&LOOK_ORIGINAL, "look controller update", gamedll.at(LOOK_UPDATE), look as UpdateFn) }.missing()?;
    VTABLE.store(gamedll.at(PLAYER_VTABLE), Relaxed);
    let names: Vec<&str> = CLIMBING_UPDATES.iter().map(|&(name, _)| name).collect();
    log!("climbing: the game says when the player climbs ({})", names.join(", "));
    Ok(())
}

/// The player behind `controller`, when it is the local player's.
fn player_of(controller: usize) -> Option<usize> {
    let vtable = VTABLE.load(Relaxed);
    (vtable != 0 && players(controller, vtable)).then(|| monaka_hook::mem::read::<usize>(controller + 8)).flatten()
}

unsafe extern "C" fn update<const I: usize>(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { ORIGINALS[I].get()(controller, dt) };
    if player_of(controller).is_some() {
        LAST.store(monaka_channel::tick(), Relaxed);
        WHICH.store(I, Relaxed);
    }
}

unsafe extern "C" fn look(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    if let Some(player) = player_of(controller) {
        crate::providers::anti_wall::hold(player, VTABLE.load(Relaxed), crate::providers::eye_site::running());
    }
    // SAFETY: forwards the game's own call.
    unsafe { LOOK_ORIGINAL.get()(controller, dt) };
}

/// Whether the player climbs now; `None` without the hooks (the hand rig then tells it from the
/// hands' height).
pub fn climbing() -> Option<bool> {
    if VTABLE.load(Relaxed) == 0 {
        return None;
    }
    let last = LAST.load(Relaxed);
    let climbing = last != 0 && monaka_channel::tick().saturating_sub(last) < FRESH_MS;
    note(climbing);
    Some(climbing)
}

/// Logs each change of whether the player climbs (the first few), with the controller.
fn note(climbing: bool) {
    static WAS: AtomicBool = AtomicBool::new(false);
    static CHANGES: AtomicU64 = AtomicU64::new(0);
    if WAS.swap(climbing, Relaxed) != climbing && CHANGES.fetch_add(1, Relaxed) < 60 {
        match climbing {
            true => log!("climbing: the {} controller runs", CLIMBING_UPDATES.get(WHICH.load(Relaxed)).map_or("?", |&(name, _)| name)),
            false => log!("climbing: done"),
        }
    }
}
