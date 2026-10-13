//! Provider: the throw release (`monaka_body::launch::Throws`): the game's throw hooked where the
//! player lets go and where what is thrown leaves, each asking the throw ready-made; and throwing
//! by hand's facts (`monaka_body::throw::ThrowsByHand`). Its component is `crate::THROW_BY_HAND`.
//!
//! The game's throw controller (`WeaponThrowController`) winds up while the trigger is held and,
//! once it is let go, plays the throw up to the animation's release time before the object leaves
//! ([`set_state`]): when the ready-made has the hands' throw (`launch::let_go`), that time is set
//! to now. The throw itself ([`throw`]) makes a flare as a dropped item ([`spawn`]), puts it at
//! the right hand and sets its velocity from the throwing direction: the flare is moved to the
//! ready-made's start and given its velocity (`launch::start`, `launch::velocity`;
//! [`Way::Given`]).
//!
//! Molotovs, grenades and the like are launched by their own object ([`launch`]) with a velocity,
//! zero for the game's own lob: that lob becomes the ready-made's, from its start
//! ([`Way::Launched`]).
//!
//! Throwing stars (the game's throwing knives) and melee weapons fly at a point instead, the
//! game's aim along the throwing direction ([`aim_point`]; [`Way::AtPoint`]): that point moves onto
//! the line of the ready-made's velocity from its start, as far away as it is fast
//! ([`REACH_PER_SPEED`], from [`NEAREST`] to [`FARTHEST`]; straight down below [`STILL`]), and the
//! stars made are moved to the start. A target the game locked on to stays when the throw is
//! within [`KEEP_TARGET`] of it. A melee weapon's throw ([`melee_throw`]) is its own let-go
//! ([`Thrown::Weapon`]), thrown at a point from where the weapon is.

use crate::Dl1;
use crate::providers::skeleton;
use eng_chr::providers::throws::ThrowSpan;
use monaka_body::launch::{self, HandThrow, Leaving, Throws, Thrown, Way};
use monaka_body::throw::{Throwing, ThrowsByHand};
use monaka_core::math::length;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// `WeaponThrowController`'s state change `(controller, state)` (it keeps the state before, the
/// time it changed and, entering a throwing state, the release time from the throw animation), and
/// its throw of the held item `(controller)` (run from its update at the release time).
const SET_STATE: usize = 0xdd33a0;
const THROW: usize = 0xdd1ad0;
/// The point a held item or melee weapon is thrown at: `(melee controller, out point, use the focus)
/// -> the target's model or 0`: the focus target's point, else the eye plus the throwing direction
/// times a range. Called by the throw, the melee weapon's throw and the throw's aim HUD.
const AIM_POINT: usize = 0xdbccc0;
/// `WeaponMeleeController`'s throw of the held weapon: `(controller)`.
const MELEE_THROW: usize = 0xdbc0b0;
/// `ThrowableObject`'s throw impulse: `(object, velocity or a zero vector for the player's own throw
/// impulse, target point or null)`; the velocity is added to the object's, with a spin.
const LAUNCH: usize = 0xeea9f0;
/// Makes the dropped world object for an inventory item: `(item, player, drop reason)`.
const SPAWN: usize = 0x6fa610;
/// In the controller: the state, and the release time into the throwing states (seconds).
const STATE: usize = 0x38;
const RELEASE_TIME: usize = 0x60;
/// The throwing states the trigger's let-go enters: 3 the throw, 4 the quick throw. State 5, the
/// throwing stars' multi-throw, also gets a release time but is not entered by the let-go.
const THROWING: [u32; 2] = [3, 4];
/// In the controller: whether it holds an item, the item; the item's object and its item type
/// (vfunc +0x1c0, the game's `EItemType`).
const HOLDS: usize = 0x90;
const HELD: usize = 0x98;
const ITEM_OBJECT: usize = 0x58;
const KIND_SLOT: usize = 0x1c0;
/// A flare: the throw makes it as a dropped item and writes its velocity at +0x100.
const FLARE: u32 = 0x1f;
/// Throwing stars (the game's throwing knives): thrown at points (the targets locked on to, or the
/// aim).
const THROWING_STAR: u32 = 0x28;
/// In a thrown object: its control object, and its velocity.
const CONTROL: usize = 0x18;
const VELOCITY: usize = 0x100;
const SET_WORLD_POSITION: &str = "?SetWorldPosition@IControlObject@@QEAAXAEBVvec3@@@Z";

/// A point thrown at is this far per m/s of the throw's speed, and this near and far (metres);
/// below [`STILL`] m/s the throw is at rest and what it held drops straight down.
const REACH_PER_SPEED: f32 = 3.0;
const NEAREST: f32 = 0.5;
const FARTHEST: f32 = 40.0;
const STILL: f32 = 0.05;
const DOWN: [f32; 3] = [0.0, -1.0, 0.0];
/// A locked target stays when the throw is within this of it (degrees).
const KEEP_TARGET: f32 = 12.0;

type SetStateFn = unsafe extern "C" fn(*mut c_void, u32);
type ThrowFn = unsafe extern "C" fn(*mut c_void);
type SpawnFn = unsafe extern "C" fn(usize, usize, u32) -> usize;
type SetPositionFn = unsafe extern "system" fn(usize, *const [f32; 3]);
type KindFn = unsafe extern "C" fn(usize) -> u32;
type AimPointFn = unsafe extern "C" fn(*mut c_void, *mut [f32; 3], u8) -> usize;
type MeleeThrowFn = unsafe extern "C" fn(*mut c_void);
type LaunchFn = unsafe extern "C" fn(usize, *const [f32; 3], *const [f32; 3]);

static SET_STATE_ORIGINAL: Original<SetStateFn> = Original::new();
static THROW_ORIGINAL: Original<ThrowFn> = Original::new();
static SPAWN_ORIGINAL: Original<SpawnFn> = Original::new();
static AIM_POINT_ORIGINAL: Original<AimPointFn> = Original::new();
static MELEE_THROW_ORIGINAL: Original<MeleeThrowFn> = Original::new();
static LAUNCH_ORIGINAL: Original<LaunchFn> = Original::new();
/// While a launched throwable's throw runs: its start and velocity.
static LAUNCHING: Mutex<Option<([f32; 3], [f32; 3])>> = Mutex::new(None);
/// A throw at a point: where it leaves from, its direction, and its reach.
type Steer = ([f32; 3], [f32; 3], f32);
/// While a throw at a point runs: its steer.
static STEER: Mutex<Option<Steer>> = Mutex::new(None);
static STEERED: AtomicU64 = AtomicU64::new(0);
static SET_POSITION: AtomicUsize = AtomicUsize::new(0);
/// The hands' throw at the latest let-go.
static RELEASE: Mutex<Option<HandThrow>> = Mutex::new(None);
/// While the player's throw runs: the objects it makes.
static MAKING: Mutex<Option<Vec<usize>>> = Mutex::new(None);
static BY_HAND: AtomicU64 = AtomicU64::new(0);
/// The player's throw under way.
static SPAN: ThrowSpan = ThrowSpan::new();

/// The throw release on the game's throw controller, launches and aim points.
impl Throws for Dl1 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        Ok(install(start.hooks, &start.build.gamedll, &start.build.engine)?)
    }

    fn throwing() -> bool {
        throwing()
    }

    fn report() {
        report();
    }
}

/// Throwing by the hand's own speed: 1.6 times it for an object given a velocity, 3 times for a
/// launched throwable (a hand's throw in a room is slower than the game's own 13 m/s lob), the
/// melee weapon at the right hand's fastest over half a second. The controllers' motion is their
/// trail on the tracking origin, and each hand is where the rig left the arms' hand
/// ([`hand_world`]): the arm the game animates through a throw lags its controller.
impl ThrowsByHand for Dl1 {
    const THROWING: Throwing = Throwing { given: 1.6, launched: 3.0, weapon_within: 0.5 };
}

/// Hooks the throw (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module) -> Result<(), Rejection> {
    if THROW_ORIGINAL.is_set() {
        return Ok(());
    }
    let set_position = engine_module.export(SET_WORLD_POSITION).ok_or_else(|| Rejection::revision("the engine does not export IControlObject::SetWorldPosition"))?;
    SET_POSITION.store(set_position, Relaxed);
    // SAFETY: each detour has its function's signature (read from its code); the game DLL's build
    // is checked by the caller; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&SET_STATE_ORIGINAL, "throw controller state", gamedll.at(SET_STATE), set_state as SetStateFn)?;
        hooks.inline_decoded(&THROW_ORIGINAL, "throw controller throw", gamedll.at(THROW), throw as ThrowFn)?;
        hooks.inline_decoded(&SPAWN_ORIGINAL, "item object spawn", gamedll.at(SPAWN), spawn as SpawnFn)?;
        hooks.inline_decoded(&AIM_POINT_ORIGINAL, "thrown item aim point", gamedll.at(AIM_POINT), aim_point as AimPointFn)?;
        hooks.inline_decoded(&MELEE_THROW_ORIGINAL, "melee weapon throw", gamedll.at(MELEE_THROW), melee_throw as MeleeThrowFn)?;
        hooks.inline_decoded(&LAUNCH_ORIGINAL, "throwable launch", gamedll.at(LAUNCH), launch as LaunchFn)?;
    }
    Ok(())
}

/// Whether the throw's hooks are on (the throw probe leaves those functions to them).
pub fn installed() -> bool {
    THROW_ORIGINAL.is_set()
}

/// Whether the player's throw controller is in a throw (winding up to letting go), or ended a
/// moment ago (`eng_chr::providers::throws::ThrowSpan`): the game then poses the next tool in the
/// right hand, held out in front.
pub fn throwing() -> bool {
    SPAN.now()
}

unsafe extern "C" fn set_state(controller: *mut c_void, state: u32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { SET_STATE_ORIGINAL.get()(controller, state) };
    let at = controller as usize;
    if !skeleton::players(at) {
        return;
    }
    SPAN.note(mem::read::<u32>(at + STATE).is_some_and(|s| s != 0));
    if !THROWING.contains(&state) || mem::read::<u32>(at + STATE) != Some(state) {
        return;
    }
    // Let go: the object leaves on the next update, with the throw the hands made now.
    let hand = launch::let_go(Thrown::Throwable);
    if let Ok(mut release) = RELEASE.lock() {
        *release = hand;
    }
    if hand.is_some() {
        mem::write::<f32>(at + RELEASE_TIME, 0.0);
    }
}

/// The kind of the item the controller holds.
fn kind(controller: usize) -> Option<u32> {
    mem::read::<usize>(controller + HOLDS).filter(|&h| h != 0)?;
    let object = mem::read::<usize>(controller + HELD).filter(|&i| i != 0)? + ITEM_OBJECT;
    let getter = mem::read::<usize>(object).and_then(|vtable| mem::read::<usize>(vtable + KIND_SLOT))?;
    Module::find(crate::providers::engine::GAMEDLL).filter(|m| m.contains(getter))?;
    // SAFETY: the item object's own kind getter, on the game thread.
    Some(unsafe { std::mem::transmute::<usize, KindFn>(getter)(object) })
}

/// The ready-made's start and velocity for an item leaving.
fn leaving(leaving: &Leaving) -> Option<([f32; 3], [f32; 3])> {
    Some((launch::start(leaving)?, launch::velocity(leaving)?))
}

unsafe extern "C" fn throw(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let players = skeleton::players(controller as usize);
    let kind = kind(controller as usize);
    let at_point = |hand: HandThrow| Leaving { thrown: Thrown::Throwable, way: Way::AtPoint, hand, game: None };
    if players && kind == Some(THROWING_STAR) {
        let thrown = RELEASE.lock().ok().and_then(|mut r| r.take()).and_then(|hand| leaving(&at_point(hand)));
        if let Ok(mut making) = MAKING.lock() {
            *making = Some(Vec::new());
        }
        // SAFETY: forwards the game's own call.
        steered(thrown, || unsafe { THROW_ORIGINAL.get()(controller) });
        let made = MAKING.lock().ok().and_then(|mut m| m.take()).unwrap_or_default();
        // A star thrown by hand leaves from the throw's start (the game makes it at the right hand).
        if let Some((start, _)) = thrown {
            for object in made {
                move_to(object, start);
            }
        }
        return;
    }
    if players && kind.is_some() && kind != Some(FLARE) {
        let hand = RELEASE.lock().ok().and_then(|mut r| r.take());
        if let Ok(mut l) = LAUNCHING.lock() {
            *l = hand.and_then(|hand| leaving(&Leaving { thrown: Thrown::Throwable, way: Way::Launched, hand, game: None }));
        }
        // SAFETY: forwards the game's own call.
        unsafe { THROW_ORIGINAL.get()(controller) };
        if let Ok(mut l) = LAUNCHING.lock() {
            *l = None;
        }
        return;
    }
    let ours = players && kind == Some(FLARE);
    if ours && let Ok(mut making) = MAKING.lock() {
        *making = Some(Vec::new());
    }
    // SAFETY: forwards the game's own call.
    unsafe { THROW_ORIGINAL.get()(controller) };
    if !ours {
        return;
    }
    let made = MAKING.lock().ok().and_then(|mut m| m.take()).unwrap_or_default();
    let hand = RELEASE.lock().ok().and_then(|mut r| r.take());
    // One object: the flare thrown by hand.
    let ([object], Some(hand)) = (made.as_slice(), hand) else { return };
    let given = Leaving { thrown: Thrown::Throwable, way: Way::Given, hand, game: mem::read::<[f32; 3]>(object + VELOCITY) };
    let start = launch::start(&given);
    if let Some(start) = start {
        move_to(*object, start);
    }
    let Some(thrown) = launch::velocity(&given) else { return };
    for (k, c) in thrown.iter().enumerate() {
        mem::write::<f32>(object + VELOCITY + 4 * k, *c);
    }
    let n = BY_HAND.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        log!("throwing by hand: throw {n} from {start:.2?} at {thrown:.1?} ({:.1} m/s)", length(thrown));
    }
}

/// Puts a new world object at `position`.
fn move_to(object: usize, position: [f32; 3]) {
    let set_position = SET_POSITION.load(Relaxed);
    if set_position != 0 {
        // SAFETY: the engine's export on the new object's control object, on the game thread.
        unsafe { std::mem::transmute::<usize, SetPositionFn>(set_position)(object + CONTROL, &position) };
    }
}

unsafe extern "C" fn launch(object: usize, velocity: *const [f32; 3], target: *const [f32; 3]) {
    let _flight = InFlight::enter();
    let thrown = LAUNCHING.lock().ok().and_then(|mut l| l.take());
    // Only the game's own lob (a zero velocity, no target) becomes the ready-made's.
    let own = target.is_null() && mem::read::<[f32; 3]>(velocity as usize).is_some_and(|v| v == [0.0; 3]);
    let Some((start, thrown)) = thrown.filter(|_| own) else {
        // SAFETY: forwards the game's own call.
        return unsafe { LAUNCH_ORIGINAL.get()(object, velocity, target) };
    };
    move_to(object, start);
    let n = BY_HAND.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        log!("throwing by hand: launch {n} from {start:.2?} at {thrown:.1?} ({:.1} m/s)", length(thrown));
    }
    // SAFETY: the game's own call with the ready-made's velocity in place of a zero one.
    unsafe { LAUNCH_ORIGINAL.get()(object, &thrown, target) };
}

unsafe extern "C" fn spawn(item: usize, player: usize, mode: u32) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let object = unsafe { SPAWN_ORIGINAL.get()(item, player, mode) };
    if object != 0
        && let Ok(mut making) = MAKING.lock()
        && let Some(made) = making.as_mut()
    {
        made.push(object);
    }
    object
}

/// The steer of a throw at a point leaving from `start` at `velocity`: along it, as far as it is
/// fast; at rest it drops.
fn steer(start: [f32; 3], velocity: [f32; 3]) -> Steer {
    let speed = length(velocity);
    let direction = if speed > STILL { velocity.map(|c| c / speed) } else { DOWN };
    (start, direction, (speed * REACH_PER_SPEED).clamp(NEAREST, FARTHEST))
}

/// Runs a throw at a point (`run`) with the throw's start and velocity, where it has them, steering
/// its aim.
fn steered(thrown: Option<([f32; 3], [f32; 3])>, run: impl FnOnce()) {
    if let Ok(mut s) = STEER.lock() {
        *s = thrown.map(|(start, velocity)| steer(start, velocity));
    }
    run();
    if let Ok(mut s) = STEER.lock() {
        *s = None;
    }
}

unsafe extern "C" fn melee_throw(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let thrown = skeleton::players(controller as usize)
        .then(|| launch::let_go(Thrown::Weapon).and_then(|hand| leaving(&Leaving { thrown: Thrown::Weapon, way: Way::AtPoint, hand, game: None })))
        .flatten();
    // SAFETY: forwards the game's own call.
    steered(thrown, || unsafe { MELEE_THROW_ORIGINAL.get()(controller) });
}

/// Where the game's aim point `point` (its target `target` locked on, or 0) goes for `steer`: none
/// keeps the game's (a locked target the throw went at), else the point on the throw's line.
fn aimed(steer: Steer, point: [f32; 3], target: usize) -> Option<[f32; 3]> {
    let (from, direction, reach) = steer;
    let to = [0, 1, 2].map(|k| point[k] - from[k]);
    let distance = length(to);
    // A target locked on to stays when the throw went at it.
    if target != 0 && distance > 1e-3 {
        let cos = (0..3).map(|k| to[k] / distance * direction[k]).sum::<f32>();
        if cos >= KEEP_TARGET.to_radians().cos() {
            return None;
        }
    }
    Some([0, 1, 2].map(|k| from[k] + direction[k] * reach))
}

unsafe extern "C" fn aim_point(controller: *mut c_void, out: *mut [f32; 3], focus: u8) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let target = unsafe { AIM_POINT_ORIGINAL.get()(controller, out, focus) };
    let steer = STEER.lock().ok().and_then(|s| *s);
    let (Some(steer), Some(point)) = (steer, mem::read::<[f32; 3]>(out as usize)) else { return target };
    let Some(aimed) = aimed(steer, point, target) else { return target };
    mem::write::<[f32; 3]>(out as usize, aimed);
    let n = STEERED.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        let (_, direction, reach) = steer;
        log!("throwing by hand: thrown at {aimed:.2?} along {direction:.2?} ({reach:.1} m; the game's point {point:.2?}, target {target:#x})");
    }
    0
}

pub fn report() {
    let (by_hand, steered) = (BY_HAND.load(Relaxed), STEERED.load(Relaxed));
    if by_hand + steered > 0 {
        log!("throwing by hand: {by_hand} throws by hand, {steered} knives or weapons aimed by hand");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The aim point the throw hooks steered to before: from the hand's release, along its motion as
    /// far as it was fast, a locked target kept when thrown at.
    fn before(release: ([f32; 3], [f32; 3]), point: [f32; 3], target: usize) -> Option<[f32; 3]> {
        let (position, velocity) = release;
        let speed = length(velocity);
        let direction = if speed > STILL { velocity.map(|c| c / speed) } else { DOWN };
        let (from, reach) = (position, (speed * REACH_PER_SPEED).clamp(NEAREST, FARTHEST));
        let to = [0, 1, 2].map(|k| point[k] - from[k]);
        let distance = length(to);
        if target != 0 && distance > 1e-3 {
            let cos = (0..3).map(|k| to[k] / distance * direction[k]).sum::<f32>();
            if cos >= KEEP_TARGET.to_radians().cos() {
                return None;
            }
        }
        Some([0, 1, 2].map(|k| from[k] + direction[k] * reach))
    }

    #[test]
    fn a_point_throw_is_steered_where_it_was_before() {
        let from = [10.0, 1.5, -4.0];
        for velocity in [[0.0; 3], [0.01, 0.0, 0.02], [0.0, 1.0, -4.0], [3.0, 0.5, -20.0], [-1.0, -2.0, 0.0]] {
            for point in [[10.0, 1.5, -14.0], [12.0, 3.5, -30.0], [10.0, 1.5, -4.0], [-5.0, 0.0, 0.0]] {
                for target in [0, 0x5000] {
                    let now = aimed(steer(from, velocity), point, target);
                    assert_eq!(now, before((from, velocity), point, target), "{velocity:?} {point:?} {target}");
                }
            }
        }
    }
}
