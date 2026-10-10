//! Throwing tools by hand (`throw_by_hand`, with the hand rig): a throwable (knife, grenade,
//! molotov, ...) leaves the moment the left trigger is let go, from the left hand that throws it,
//! with the left controller's fastest velocity over the last [`RELEASE_WINDOW`] (the arm the game
//! animates through a throw lags the controller). While a throw runs the left
//! hand stays on its controller ([`throwing_now`]): the throw's animation brings it near the
//! weapon, where the rig would otherwise take it for a two-handed hold and move it with the right.
//! The game holds the tool in the right hand's weapon slot for its throw: it is shown in the left
//! hand ([`place_tool`]).
//!
//! The game's throw controller (`WeaponThrowController`) winds up while the trigger is held and,
//! once it is let go, plays the throw up to the animation's release time before the object leaves
//! ([`set_state`]): that time is set to now. The throw itself ([`throw`]) makes the object from the
//! item ([`spawn`]), puts it at the right hand and sets its velocity from the view: the object is
//! moved to the throwing hand and given the hand's velocity at the release times [`GAIN`]. The hand
//! decides the throw however slow it is: a release with no motion just drops what it held.
//!
//! Molotovs, grenades and the like are launched by their own object ([`launch`]) with a velocity,
//! zero for the game's own lob: a throw by hand gives it the hand's instead, from the hand.
//!
//! Throwing knives and melee weapons fly at a point instead, the game's aim along the view
//! ([`aim_point`]): a throw by hand moves that point onto the line of the hand's throw (straight
//! down for a hand at rest), as far away as the hand was fast ([`REACH_PER_SPEED`], from
//! [`NEAREST`] to [`FARTHEST`]). A target the game locked on to stays
//! when the hand threw within [`KEEP_TARGET`] of it. A melee weapon's throw ([`melee_throw`])
//! comes a little after the swing that starts it: the fastest of the right hand's last
//! [`SWING_WINDOW`] is the throw.

use crate::player::{hand_world, hands};
use monaka_core::camera::Mat34;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// `WeaponThrowController`: its state change `(controller, state)`, its throw `(controller)`.
const SET_STATE: usize = 0xdd33a0;
const THROW: usize = 0xdd1ad0;
/// A thrown item's aim point: `(controller, out point, use the focus) -> the target's model or 0`.
const AIM_POINT: usize = 0xdbccc0;
/// `WeaponMeleeController`'s throw of the held weapon: `(controller)`.
const MELEE_THROW: usize = 0xdbc0b0;
/// `ThrowableObject`'s launch: `(object, velocity or a zero vector for its own, target point or
/// null)`; the velocity is added to the object's.
const LAUNCH: usize = 0xeea9f0;
/// Makes the world object for an inventory item: `(item, player, mode)`.
const SPAWN: usize = 0x6fa610;
/// In the controller: the state, and the release time into states 3 and 4 (seconds).
const STATE: usize = 0x38;
const RELEASE_TIME: usize = 0x60;
const THROWING: [u32; 2] = [3, 4];
/// The controller's player (its vfunc +0x20).
const PLAYER_SLOT: usize = 0x20;
/// In the controller: whether it holds an item, the item; the item's object and its kind (vfunc
/// +0x1c0): 0x1f a thrown object (the one kind whose velocity the throw writes at +0x100).
const HOLDS: usize = 0x90;
const HELD: usize = 0x98;
const ITEM_OBJECT: usize = 0x58;
const KIND_SLOT: usize = 0x1c0;
const THROWN_OBJECT: u32 = 0x1f;
/// Throwing knives: thrown at points (the targets locked on to, or the aim).
const KNIVES: u32 = 0x28;
/// In a thrown object: its control object, and its velocity.
const CONTROL: usize = 0x18;
const VELOCITY: usize = 0x100;
const SET_WORLD_POSITION: &str = "?SetWorldPosition@IControlObject@@QEAAXAEBVvec3@@@Z";

/// The hand speed carried into the throw.
const GAIN: f32 = 1.6;
/// The hand that throws tools, and the one holding a melee weapon.
const HAND: usize = hand_world::LEFT;
const WEAPON_HAND: usize = hand_world::RIGHT;
/// How far back a release looks for the throwing hand's swing.
const RELEASE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
/// A point thrown at by hand is this far per m/s of the hand's speed, and this near and far
/// (metres); below [`STILL`] m/s the hand is at rest and what it held drops straight down.
const REACH_PER_SPEED: f32 = 3.0;
const NEAREST: f32 = 0.5;
const FARTHEST: f32 = 40.0;
const STILL: f32 = 0.05;
const DOWN: [f32; 3] = [0.0, -1.0, 0.0];
/// A locked target stays when the hand's throw is within this of it (degrees).
const KEEP_TARGET: f32 = 12.0;
/// How far back a melee weapon's throw looks for its swing.
const SWING_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

type SetStateFn = unsafe extern "C" fn(*mut c_void, u32);
type ThrowFn = unsafe extern "C" fn(*mut c_void);
type SpawnFn = unsafe extern "C" fn(usize, usize, u32) -> usize;
type SetPositionFn = unsafe extern "system" fn(usize, *const [f32; 3]);
type GetterFn = unsafe extern "system" fn(*mut c_void) -> usize;
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
/// While a throw by hand of a launched throwable runs: where the hand let go and its velocity.
static LAUNCHING: Mutex<Option<([f32; 3], [f32; 3])>> = Mutex::new(None);
/// A throw by hand at a point: where the hand let go, the throw's direction, and its reach.
type Steer = ([f32; 3], [f32; 3], f32);
/// While a throw by hand at a point runs: its steer.
static STEER: Mutex<Option<Steer>> = Mutex::new(None);
static STEERED: AtomicU64 = AtomicU64::new(0);
static SET_POSITION: AtomicUsize = AtomicUsize::new(0);
/// The hand's position and velocity at the latest release.
static RELEASE: Mutex<Option<([f32; 3], [f32; 3])>> = Mutex::new(None);
/// While the player's throw runs: the objects it makes.
static MAKING: Mutex<Option<Vec<usize>>> = Mutex::new(None);
static BY_HAND: AtomicU64 = AtomicU64::new(0);
static TOOL_PLACED: AtomicU64 = AtomicU64::new(0);
static RELEASES: AtomicU64 = AtomicU64::new(0);
/// When the player's throw controller left its idle state, while it is out of it.
static THROW_STARTED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const LONGEST_THROW: std::time::Duration = std::time::Duration::from_secs(6);

pub fn install(hooks: &mut Hooks, gamedll: &Module, engine_module: &Module) -> Result<(), Rejection> {
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
    log!("throwing by hand: a tool leaves as the left trigger is let go, with the left hand's throw");
    Ok(())
}

/// Whether the controller is the player's (the arms the rig poses).
fn players(controller: usize) -> bool {
    let ours = hands::arms_model();
    let getter = mem::read::<usize>(controller).and_then(|vtable| mem::read::<usize>(vtable + PLAYER_SLOT));
    let Some(getter) = getter.filter(|&g| Module::find(crate::engine::GAMEDLL).is_some_and(|m| m.contains(g))) else { return false };
    // SAFETY: the controller's own player getter, on the game thread.
    ours != 0 && unsafe { std::mem::transmute::<usize, GetterFn>(getter)(controller as *mut c_void) } == ours
}

unsafe extern "C" fn set_state(controller: *mut c_void, state: u32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { SET_STATE_ORIGINAL.get()(controller, state) };
    let at = controller as usize;
    if !players(at) {
        return;
    }
    let busy = mem::read::<u32>(at + STATE).is_some_and(|s| s != 0);
    if let Ok(mut started) = THROW_STARTED.lock() {
        *started = match (busy, *started) {
            (false, _) => None,
            (true, Some(at)) => Some(at),
            (true, None) => Some(std::time::Instant::now()),
        };
    }
    if !THROWING.contains(&state) || mem::read::<u32>(at + STATE) != Some(state) {
        return;
    }
    // Let go: the object leaves on the next update, from where the throwing hand is now.
    let hand = throwing_hand();
    let n = RELEASES.fetch_add(1, Relaxed) + 1;
    if n <= 30 {
        let speed = |fastest: fn(usize, std::time::Duration) -> Option<[f32; 3]>| fastest(hand_world::LEFT, RELEASE_WINDOW).map_or(f32::NAN, hand_world::length);
        log!(
            "throwing by hand: release {n} (state {state}): fastest over {RELEASE_WINDOW:?}: the left controller {:.2} m/s, the left hand {:.2} m/s",
            speed(hand_world::controller_fastest),
            speed(hand_world::fastest)
        );
    }
    if let Ok(mut release) = RELEASE.lock() {
        *release = hand;
    }
    if hand.is_some() {
        mem::write::<f32>(at + RELEASE_TIME, 0.0);
    }
}

/// The throwing hand's position and its fastest velocity lately.
fn throwing_hand() -> Option<([f32; 3], [f32; 3])> {
    hand_world::position(HAND).zip(hand_world::controller_fastest(HAND, RELEASE_WINDOW))
}

/// The left hand's bones mirror the right's across their local x-z plane.
const MIRROR: Mat34 = [1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
/// A tool this near the right hand's holder is where the game put it this update (else it is still
/// where it was moved to, and moving it again would carry it off).
const AT_HOLDER: f32 = 0.05;

/// After the rig placed the arms: while a throw runs, the tool (in the right hand's weapon slot)
/// in the left hand instead, held as the right holds it, mirrored.
pub fn place_tool(vis: usize) {
    use monaka_arms::Skeleton;
    use monaka_core::camera::{compose, rigid_inverse};
    if !SET_STATE_ORIGINAL.is_set() || !throwing_now() {
        return;
    }
    let Some(tool) = crate::player::hands::weapon_model(vis, 0) else { return };
    let (Some(left), Some(right), Some(holder), Some(world)) = (hand_world::element(c"L_Hand"), hand_world::element(c"R_Hand"), hand_world::element(c"R_HandHolder"), tool.world(0)) else { return };
    let at = |m: &Mat34| [m[3], m[7], m[11]];
    let apart = hand_world::length([0, 1, 2].map(|k| at(&world)[k] - at(&holder)[k]));
    if apart > AT_HOLDER {
        static SKIPPED: AtomicU64 = AtomicU64::new(0);
        if SKIPPED.fetch_add(1, Relaxed) < 5 {
            log!("throwing by hand: the tool is {apart:.3} m from the right hand's holder: left where it is");
        }
        return;
    }
    // The right holder against its hand, mirrored into the left hand; the tool moved as the holder.
    let (left, right, holder) = (hand_world::rigid(&left), hand_world::rigid(&right), hand_world::rigid(&holder));
    let in_hand = compose(&rigid_inverse(&right), &holder);
    let target = compose(&left, &compose(&MIRROR, &compose(&in_hand, &MIRROR)));
    let moved = compose(&compose(&target, &rigid_inverse(&holder)), &world);
    tool.set_world(0, &moved);
    let n = TOOL_PLACED.fetch_add(1, Relaxed) + 1;
    if n == 1 {
        log!("throwing by hand: the tool shown in the left hand while it is thrown");
    }
}

/// Whether the player's throw controller is in a throw (winding up to letting go), for at most
/// [`LONGEST_THROW`]: a throw cut short (the tool put away) may never say it ended.
pub fn throwing_now() -> bool {
    THROW_STARTED.lock().ok().and_then(|s| *s).is_some_and(|at| at.elapsed() < LONGEST_THROW)
}

/// The kind of the item the controller holds.
fn kind(controller: usize) -> Option<u32> {
    mem::read::<usize>(controller + HOLDS).filter(|&h| h != 0)?;
    let object = mem::read::<usize>(controller + HELD).filter(|&i| i != 0)? + ITEM_OBJECT;
    let getter = mem::read::<usize>(object).and_then(|vtable| mem::read::<usize>(vtable + KIND_SLOT))?;
    Module::find(crate::engine::GAMEDLL).filter(|m| m.contains(getter))?;
    // SAFETY: the item object's own kind getter, on the game thread.
    Some(unsafe { std::mem::transmute::<usize, KindFn>(getter)(object) })
}

unsafe extern "C" fn throw(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let players = players(controller as usize);
    let kind = kind(controller as usize);
    if players && kind == Some(KNIVES) {
        let release = RELEASE.lock().ok().and_then(|mut r| r.take());
        if let Ok(mut making) = MAKING.lock() {
            *making = Some(Vec::new());
        }
        // SAFETY: forwards the game's own call.
        steered(release, || unsafe { THROW_ORIGINAL.get()(controller) });
        let made = MAKING.lock().ok().and_then(|mut m| m.take()).unwrap_or_default();
        // A knife thrown by hand leaves from the hand (the game makes it at the right one).
        if let Some((position, _)) = release {
            for object in made {
                move_to(object, position);
            }
        }
        return;
    }
    if players && kind.is_some() && kind != Some(THROWN_OBJECT) {
        let release = RELEASE.lock().ok().and_then(|mut r| r.take());
        if let Ok(mut l) = LAUNCHING.lock() {
            *l = release.map(|(position, velocity)| (position, velocity.map(|c| c * GAIN)));
        }
        // SAFETY: forwards the game's own call.
        unsafe { THROW_ORIGINAL.get()(controller) };
        if let Ok(mut l) = LAUNCHING.lock() {
            *l = None;
        }
        return;
    }
    let ours = players && kind == Some(THROWN_OBJECT);
    if ours && let Ok(mut making) = MAKING.lock() {
        *making = Some(Vec::new());
    }
    // SAFETY: forwards the game's own call.
    unsafe { THROW_ORIGINAL.get()(controller) };
    if !ours {
        return;
    }
    let made = MAKING.lock().ok().and_then(|mut m| m.take()).unwrap_or_default();
    let release = RELEASE.lock().ok().and_then(|mut r| r.take());
    // One object: a tool thrown by hand (the knives' throw at several targets keeps the game's aim).
    let ([object], Some((position, velocity))) = (made.as_slice(), release) else { return };
    let speed = hand_world::length(velocity);
    move_to(*object, position);
    let thrown = velocity.map(|c| c * GAIN);
    for (k, c) in thrown.iter().enumerate() {
        mem::write::<f32>(object + VELOCITY + 4 * k, *c);
    }
    let n = BY_HAND.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        log!("throwing by hand: throw {n} from {position:.2?} at {thrown:.1?} ({:.1} m/s)", speed * GAIN);
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
    let hand = LAUNCHING.lock().ok().and_then(|mut l| l.take());
    // Only the game's own lob (a zero velocity, no target) becomes the hand's.
    let own = target.is_null() && mem::read::<[f32; 3]>(velocity as usize).is_some_and(|v| v == [0.0; 3]);
    let Some((position, thrown)) = hand.filter(|_| own) else {
        // SAFETY: forwards the game's own call.
        return unsafe { LAUNCH_ORIGINAL.get()(object, velocity, target) };
    };
    move_to(object, position);
    let n = BY_HAND.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
        log!("throwing by hand: launch {n} from {position:.2?} at {thrown:.1?} ({:.1} m/s)", hand_world::length(thrown));
    }
    // SAFETY: the game's own call with the hand's velocity in place of a zero one.
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

/// Runs a throw at a point with the hand's release steering its aim (a fast enough one).
fn steered(release: Option<([f32; 3], [f32; 3])>, run: impl FnOnce()) {
    // Along the hand's motion, as far as it was fast; a hand at rest drops it.
    let steer = release.map(|(position, velocity)| {
        let speed = hand_world::length(velocity);
        let direction = if speed > STILL { velocity.map(|c| c / speed) } else { DOWN };
        (position, direction, (speed * REACH_PER_SPEED).clamp(NEAREST, FARTHEST))
    });
    if let Ok(mut s) = STEER.lock() {
        *s = steer;
    }
    run();
    if let Ok(mut s) = STEER.lock() {
        *s = None;
    }
}

unsafe extern "C" fn melee_throw(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let release = players(controller as usize)
        .then(|| hand_world::position(WEAPON_HAND).zip(hand_world::controller_fastest(WEAPON_HAND, SWING_WINDOW)))
        .flatten();
    // SAFETY: forwards the game's own call.
    steered(release, || unsafe { MELEE_THROW_ORIGINAL.get()(controller) });
}

unsafe extern "C" fn aim_point(controller: *mut c_void, out: *mut [f32; 3], focus: u8) -> usize {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let target = unsafe { AIM_POINT_ORIGINAL.get()(controller, out, focus) };
    let steer = STEER.lock().ok().and_then(|s| *s);
    let (Some((from, direction, reach)), Some(point)) = (steer, mem::read::<[f32; 3]>(out as usize)) else { return target };
    let to = [0, 1, 2].map(|k| point[k] - from[k]);
    let distance = hand_world::length(to);
    // A target locked on to stays when the hand threw at it.
    if target != 0 && distance > 1e-3 {
        let cos = (0..3).map(|k| to[k] / distance * direction[k]).sum::<f32>();
        if cos >= KEEP_TARGET.to_radians().cos() {
            return target;
        }
    }
    let aimed = [0, 1, 2].map(|k| from[k] + direction[k] * reach);
    mem::write::<[f32; 3]>(out as usize, aimed);
    let n = STEERED.fetch_add(1, Relaxed) + 1;
    if n <= 20 {
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

/// Whether the throw's hooks are on (the throw probe leaves those functions to them).
pub fn installed() -> bool {
    THROW_ORIGINAL.is_set()
}
