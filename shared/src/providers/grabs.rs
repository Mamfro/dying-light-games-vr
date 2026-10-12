//! Provider: the may-be-grabbed conditions' hooks (the option is
//! `crate::feature::DISABLE_ZOMBIE_GRABS`, its component `crate::feature::grabs`).
//!
//! Disabled zombie grabs (`disable_zombie_grabs`, on by default): grabs never land on the player,
//! whatever aims.
//!
//! A grab is a sync action (`sync_actions_grabs.def`), started only when all its conditions hold
//! for its participants (0 the victim, 1 the attacker). Three of those conditions single out the
//! grabs on the player, and each answers "no" here when its participant is a player:
//!
//! - `IsBusy(0, false)` (the victim not busy), asked by the biters' front grabs, the crawlers', the
//!   volatiles' and the demolishers' grabs, the virals' power attack and the humans' headbutt, and
//!   by no other sync action: the player is answered busy;
//! - `InClimb(0, true)` (the victim climbing), asked only by the virals' grabs off a ledge (other
//!   sync actions ask `InClimb(0, false)`, left alone);
//! - `MinDistance(0, 1, 1.1)`, asked only by the biter's grab of a player running past it.
//!
//! Other characters are grabbed as in the game. A player is told by the class of the participant
//! the condition asks about ([`is_player`]). Landing on a zombie from a fall (the "fall absorb"
//! sync actions) is the player's move and stays.
//!
//! In VR the reach of a grab is hard to judge (a biter lunges for one from a few metres away), so
//! grabs land far more often than on a flat screen.

use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem, probe};
use monaka_producer::{Rejection, log};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One build's sync action condition checks, each `(condition, participants) -> passes` (game DLL
/// RVAs: the conditions' vtable slot 1).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// `IsBusy`.
    pub is_busy: usize,
    /// `InClimb` (`SyncActionConditionIsClimbing`).
    pub in_climb: usize,
    /// `MinDistance`.
    pub min_distance: usize,
}

/// The condition's (first) participant index (i32); a yes-or-no condition's wanted answer (u8);
/// a distance condition's distance (f32).
const PARTICIPANT: usize = 0x08;
const WANTS_YES: usize = 0x0c;
const DISTANCE: usize = 0x10;
/// The distance only the run-past grab asks for (metres).
const RUN_PAST: f32 = 1.1;
/// The participants: a packed pointer (low 48 bits) to entries of an entity handle and an actor.
const ENTRIES: usize = 0x08;
const ENTRY: usize = 0x10;
const ACTOR: usize = 0x08;
/// The actor's vtable slot that gives the state the check reads.
const STATE_SLOT: usize = 0x70;

type CheckFn = unsafe extern "C" fn(usize, usize) -> bool;

static IS_BUSY_ORIGINAL: Original<CheckFn> = Original::new();
static IN_CLIMB_ORIGINAL: Original<CheckFn> = Original::new();
static MIN_DISTANCE_ORIGINAL: Original<CheckFn> = Original::new();
static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Each participant class seen, by vtable: whether it is a player's (none: no class to read).
static CLASSES: Mutex<Option<HashMap<usize, Option<bool>>>> = Mutex::new(None);
/// The game DLL, whose code alone is called on a participant.
static GAMEDLL: OnceLock<Module> = OnceLock::new();

/// Hooks `build`'s three condition checks in the game DLL.
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    let _ = GAMEDLL.set(*gamedll);
    // SAFETY: the detours have the checks' signature (as every sync action condition's check: the
    // condition and the participants, a bool back); the game DLL's build is checked by the caller;
    // the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&IS_BUSY_ORIGINAL, "sync action is-busy check", gamedll.at(build.is_busy), is_busy as CheckFn)?;
        hooks.inline_decoded(&IN_CLIMB_ORIGINAL, "sync action in-climb check", gamedll.at(build.in_climb), in_climb as CheckFn)?;
        hooks.inline_decoded(&MIN_DISTANCE_ORIGINAL, "sync action min-distance check", gamedll.at(build.min_distance), min_distance as CheckFn)?;
    }
    log!("zombie grabs: disabled (disable_zombie_grabs=0 lets them grab again)");
    Ok(())
}

/// The player's own classes whose RTTI name does not say "Player": the player's sync action
/// controller (a `PlayerController`), which a grab's participants hold for the player.
const PLAYER_CLASSES: &[&str] = &[".?AVSyncActionController@@"];

/// Whether `object` (a C++ object, possibly a base in the middle of one) is a player's: its class's
/// RTTI name says "Player" (`PlayerDI_PH` and the player's own parts; the AI's are named for the
/// AI) or is one of [`PLAYER_CLASSES`]. None when it has no class to read (not such an object).
/// Each class is looked up once and logged.
fn is_player(object: usize) -> Option<bool> {
    let vtable = mem::read::<usize>(object).filter(|&v| v != 0)?;
    let mut classes = CLASSES.lock().ok()?;
    let classes = classes.get_or_insert_with(HashMap::new);
    if let Some(&known) = classes.get(&vtable) {
        return known;
    }
    let name = probe::class_name(object);
    let player = name.as_deref().map(|n| n.contains("Player") || PLAYER_CLASSES.contains(&n));
    if classes.len() < 40 {
        let what = match player {
            Some(true) => "a player: refused",
            Some(false) => "not a player",
            None => "no class: left alone",
        };
        log!("zombie grabs: a grab's victim of class {} is {what}", name.as_deref().unwrap_or("?"));
    }
    classes.insert(vtable, player);
    player
}

/// The actor (and the state object it gives) of participant `index` in `participants`.
fn victim_is_player(participants: usize, index: i32) -> bool {
    let Some(entries) = mem::read::<usize>(participants + ENTRIES).map(|p| p & 0xffff_ffff_ffff).filter(|&p| p != 0) else { return false };
    let Ok(index) = usize::try_from(index) else { return false };
    let Some(actor) = mem::read::<usize>(entries + index * ENTRY + ACTOR).filter(|&a| a != 0) else { return false };
    match is_player(actor) {
        Some(true) => return true,
        // Not a C++ object with a class: the participants are not laid out as read here, and
        // nothing of it is called.
        None => return false,
        Some(false) => {}
    }
    let getter = mem::read::<usize>(actor).and_then(|vtable| mem::read::<usize>(vtable + STATE_SLOT));
    let Some(getter) = getter.filter(|&g| GAMEDLL.get().is_some_and(|m| m.contains(g))) else { return false };
    // SAFETY: the actor's own state getter (an object with a class, its slot the game DLL's code),
    // as the check itself calls it on this thread.
    let state = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(actor) };
    state != 0 && is_player(state) == Some(true)
}

/// Whether `condition`'s participant is a player (counting and logging the refusal when it is).
fn refuses(condition: usize, participants: usize, what: &str) -> bool {
    let index = mem::read::<i32>(condition + PARTICIPANT).unwrap_or(-1);
    if !victim_is_player(participants, index) {
        return false;
    }
    let n = REFUSED.fetch_add(1, Relaxed) + 1;
    if n <= 20 || n.is_power_of_two() {
        log!("zombie grabs: grab {n} refused ({what})");
    }
    true
}

unsafe extern "C" fn is_busy(condition: usize, participants: usize) -> bool {
    let _flight = InFlight::enter();
    if refuses(condition, participants, "a grab") {
        // As if the player were busy: the condition passes only where it wants busy.
        return mem::read::<u8>(condition + WANTS_YES) == Some(1);
    }
    // SAFETY: forwards the game's own call.
    unsafe { IS_BUSY_ORIGINAL.get()(condition, participants) }
}

unsafe extern "C" fn in_climb(condition: usize, participants: usize) -> bool {
    let _flight = InFlight::enter();
    if mem::read::<u8>(condition + WANTS_YES) == Some(1) && refuses(condition, participants, "a grab off a ledge") {
        return false;
    }
    // SAFETY: forwards the game's own call.
    unsafe { IN_CLIMB_ORIGINAL.get()(condition, participants) }
}

unsafe extern "C" fn min_distance(condition: usize, participants: usize) -> bool {
    let _flight = InFlight::enter();
    if mem::read::<f32>(condition + DISTANCE).is_some_and(|d| (d - RUN_PAST).abs() < 1e-3) && refuses(condition, participants, "a grab running past") {
        return false;
    }
    // SAFETY: forwards the game's own call.
    unsafe { MIN_DISTANCE_ORIGINAL.get()(condition, participants) }
}

pub fn report() {
    let n = REFUSED.load(Relaxed);
    if n > 0 {
        log!("zombie grabs: {n} grabs refused");
    }
}
