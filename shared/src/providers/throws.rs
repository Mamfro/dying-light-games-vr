//! Provider part: the throw release (`monaka_arms::launch::Throws`), what Dying Light 2 and The
//! Beast share, with their throwing by hand ([`THROWING`], `monaka_arms::throw::ThrowsByHand`);
//! and the throw's span ([`ThrowSpan`]), which Dying Light 1 keeps too.
//!
//! The game's throw controller (`WeaponThrowController`) runs the throw as states (idle, wind-up,
//! held, three throw states, recover): letting go starts the throw animation, and the animation's
//! release event makes the thrown object. At that moment the controller asks two of its own
//! functions for the object's start point (the eye, a little ahead) and its velocity (the look
//! direction times the item's look impulse, plus up times its up impulse). Here ([`install`]), when
//! letting go takes the controller into a throw state, the throw ready-made is asked for the hands'
//! throw (`launch::let_go`) and the release is made at once (the game makes it once per throw, so
//! the animation's later event does nothing more); during that release the two functions answer
//! the ready-made's start and velocity (`launch::start`, `launch::velocity`, given the game's own
//! velocity). Knives, stars and spears fly as projectiles that keep this speed; bombs and props as
//! physics objects given it.
//!
//! The game's trajectory arc while the throwable is held (where the look's throw would land) says
//! nothing of where the hand will throw it: it is off during each of the player's throw updates
//! (the player parameter `ThrowParabolaEnabled`, put back after).
//!
//! A melee weapon thrown ([`install_weapon`]): the game throws it as an attack (the throw button
//! held, then its animation); at the animation's release the weapon becomes a dropped item, and the
//! dropped item's launch gives it a velocity once: a fixed speed (or the player's throw power),
//! along the player's aim, or solved toward an aim point. Right after a launch of an item the
//! player threw, the ready-made is asked for the throw (its let-go comes with the launch, after
//! the swing) and its velocity replaces the game's; the weapon starts from where it is, in the hand.

use crate::feature::Chrome;
use monaka_arms::launch::{self, HandThrow, Leaving, Thrown, Way};
use monaka_arms::throw::Throwing;
use monaka_core::math;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Missing, Start};
use monaka_producer::{Rejection, log};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One build's throw controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The controller's update `(controller, dt)`.
    pub update: usize,
    /// The thrown object's velocity `(controller, out vec3) -> out`.
    pub velocity: usize,
    /// The thrown object's start point `(controller, out vec3) -> out`.
    pub start: usize,
    /// The release `(controller)`: makes the thrown object, once per throw.
    pub release: usize,
    /// The controller's state (i32): [`IDLE`], [`THROW_STATES`].
    pub state: usize,
    /// Set once this throw's release has been made (u8).
    pub released: usize,
    /// The hand the game throws with (u32: 0 the right hand's holder, 1 the left's), logged.
    pub hand: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
    /// The player's vtable slot that returns its live parameter block, and in it
    /// `ThrowParabolaEnabled` (u8).
    pub parameters_slot: usize,
    pub arc: usize,
}

/// One build's dropped-item launch (game DLL RVA and the dropped item's fields).
#[derive(Clone, Copy, Debug)]
pub struct WeaponBuild {
    /// The launch `(dropped item, toss)`.
    pub launch: usize,
    /// The launch velocity (vec3).
    pub velocity: usize,
    /// Set when the item was thrown, not dropped (u8).
    pub thrown: usize,
    /// The owner: a pointer into the player at [`OWNER_PART`].
    pub owner: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// Every controller's player (the controllers' own eye and look read it there).
const PLAYER: usize = 0x08;
/// The part of the player an item's owner pointer points at.
const OWNER_PART: usize = 0x100;
/// The controller's idle state, and the states of a throw under way (after the button is let go).
const IDLE: i32 = -1;
const THROW_STATES: std::ops::RangeInclusive<i32> = 2..=4;
/// A throw counts as under way for at most this long (one cut short, the tool put away, may never
/// say it ended), and this long after it ended (the game then poses the next tool).
const LONGEST_THROW: Duration = Duration::from_secs(6);
const AFTER_THROW: Duration = Duration::from_millis(1500);

/// When a player's throw controller left idle (while it is out of it) and when it last went back
/// to it: whether a throw is under way, for any game's throw hooks.
pub struct ThrowSpan {
    started: Mutex<Option<Instant>>,
    ended: Mutex<Option<Instant>>,
}

impl ThrowSpan {
    pub const fn new() -> Self {
        Self { started: Mutex::new(None), ended: Mutex::new(None) }
    }

    /// The controller is out of idle (`busy`), or back in it.
    pub fn note(&self, busy: bool) {
        let Ok(mut started) = self.started.lock() else { return };
        if !busy
            && started.is_some()
            && let Ok(mut ended) = self.ended.lock()
        {
            *ended = Some(Instant::now());
        }
        *started = match (busy, *started) {
            (false, _) => None,
            (true, Some(at)) => Some(at),
            (true, None) => Some(Instant::now()),
        };
    }

    /// Under way (winding up to recovering, for at most [`LONGEST_THROW`]), or ended less than
    /// [`AFTER_THROW`] ago.
    pub fn now(&self) -> bool {
        let fresh = |at: &Mutex<Option<Instant>>, within: Duration| at.lock().ok().and_then(|s| *s).is_some_and(|at| at.elapsed() < within);
        fresh(&self.started, LONGEST_THROW) || fresh(&self.ended, AFTER_THROW)
    }
}

impl Default for ThrowSpan {
    fn default() -> Self {
        Self::new()
    }
}

type UpdateFn = unsafe extern "C" fn(usize, f32);
type VectorFn = unsafe extern "C" fn(usize, *mut [f32; 3]) -> *mut [f32; 3];
type ReleaseFn = unsafe extern "C" fn(usize);
type LaunchFn = unsafe extern "C" fn(usize, u8);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static VELOCITY_ORIGINAL: Original<VectorFn> = Original::new();
static START_ORIGINAL: Original<VectorFn> = Original::new();
static LAUNCH_ORIGINAL: Original<LaunchFn> = Original::new();

/// The build, and the release and player vtable relocated.
static BUILD: OnceLock<(Build, usize, usize)> = OnceLock::new();
/// The weapon build, and the player vtable relocated.
static WEAPON_BUILD: OnceLock<(WeaponBuild, usize)> = OnceLock::new();
static RELEASES: AtomicU64 = AtomicU64::new(0);
static SPAN: ThrowSpan = ThrowSpan::new();

thread_local! {
    /// The throw being released by us now: the hands' throw (world).
    static RELEASING: Cell<Option<HandThrow>> = const { Cell::new(None) };
}

/// Hooks `build`'s throw update, velocity and start point in the game DLL (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if UPDATE_ORIGINAL.is_set() {
        return Ok(());
    }
    let _ = BUILD.set((build, gamedll.at(build.release), gamedll.at(build.player_vtable)));
    // SAFETY: the detours have the targets' signatures (as their code and call sites use them);
    // the game DLL's build is checked by the caller; prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&UPDATE_ORIGINAL, "throw update", gamedll.at(build.update), update as UpdateFn)?;
        hooks.inline_decoded(&VELOCITY_ORIGINAL, "throw velocity", gamedll.at(build.velocity), velocity as VectorFn)?;
        hooks.inline_decoded(&START_ORIGINAL, "throw start point", gamedll.at(build.start), start as VectorFn)?;
    }
    Ok(())
}

/// Hooks `build`'s dropped-item launch in the game DLL (once).
pub fn install_weapon(hooks: &mut Hooks, gamedll: &Module, build: WeaponBuild) -> Result<(), Rejection> {
    if LAUNCH_ORIGINAL.is_set() {
        return Ok(());
    }
    let _ = WEAPON_BUILD.set((build, gamedll.at(build.player_vtable)));
    // SAFETY: the detour has the launch's signature (as its call sites use it); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&LAUNCH_ORIGINAL, "dropped item launch", gamedll.at(build.launch), launch as LaunchFn)? };
    Ok(())
}

/// Game `G`'s throw release (its `Throws::install`): the throw and the melee weapon's, as far as
/// its build has been read for them.
pub fn install_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let builds = G::FACTS.hands.and_then(|h| h.throw.map(|throw| (throw, h.weapon_throw)));
    let (throw, weapon) = builds.ok_or_else(|| Lack::Missing("the throw is not read for this build".into()))?;
    let gamedll = crate::feature::gamedll(start)?;
    install(start.hooks, gamedll, throw).missing()?;
    if let Some(weapon) = weapon {
        install_weapon(start.hooks, gamedll, weapon).missing()?;
    }
    Ok(())
}

/// Throwing by hand on the Chrome games (a game's `ThrowsByHand::THROWING`), on Dying Light 1's
/// numbers: every item the game throws here is given a velocity ([`Way::Given`]), the hand's speed
/// times 1.6 (a hand's throw in a room is slower than the game's own), knives and bombs alike (the
/// velocity's hook does not tell them apart); the melee weapon at the right hand's fastest over half
/// a second. A throwable leaves from the left palm. Nothing is launched on its own here, so the
/// launched gain goes unused.
pub const THROWING: Throwing = Throwing { given: 1.6, launched: 3.0, weapon_within: 0.5 };

/// Whether the throw's hooks are in.
pub fn installed() -> bool {
    BUILD.get().is_some()
}

/// Whether the melee weapon's throw is hooked.
pub fn weapon_installed() -> bool {
    WEAPON_BUILD.get().is_some()
}

/// Whether `controller` is the local player's (its player is a `PlayerDI_PH`).
pub fn players(controller: usize, player_vtable: usize) -> bool {
    mem::read::<usize>(controller + PLAYER).filter(|&p| p != 0).and_then(mem::read::<usize>) == Some(player_vtable)
}

/// Whether the player's throw is under way (winding up to recovering), or ended less than
/// [`AFTER_THROW`] ago (a game's `Throws::throwing`).
pub fn throwing() -> bool {
    installed() && SPAN.now()
}

/// The player's `ThrowParabolaEnabled` flag (its address) behind `controller`, the player's.
fn arc_flag(controller: usize, build: &Build) -> Option<usize> {
    let player = mem::read::<usize>(controller + PLAYER)?;
    let getter = mem::read::<usize>(player).and_then(|vtable| mem::read::<usize>(vtable + build.parameters_slot)).filter(|&g| g != 0)?;
    // SAFETY: the player's own parameter getter, no arguments beyond it, on the game thread, as the
    // throw update itself calls it.
    let parameters = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(getter)(player) };
    (parameters != 0).then_some(parameters + build.arc)
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    let Some(&(build, release, player_vtable)) = BUILD.get() else {
        // SAFETY: forwards the game's own call.
        return unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    };
    if !players(controller, player_vtable) {
        // SAFETY: forwards the game's own call.
        return unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    }
    // The arc off for this update only.
    let arc = arc_flag(controller, &build).and_then(|flag| mem::read::<u8>(flag).map(|was| (flag, was)));
    if let Some((flag, _)) = arc {
        mem::write::<u8>(flag, 0);
    }
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    if let Some((flag, was)) = arc {
        mem::write::<u8>(flag, was);
    }
    let state = mem::read::<i32>(controller + build.state).unwrap_or(IDLE);
    SPAN.note(state != IDLE);
    let released = mem::read::<u8>(controller + build.released).unwrap_or(1) != 0;
    if !THROW_STATES.contains(&state) || released {
        return;
    }
    let Some(hand) = launch::let_go(Thrown::Throwable) else { return };
    if RELEASES.fetch_add(1, Relaxed) < 40 {
        let game_hand = if mem::read::<u32>(controller + build.hand) == Some(1) { "left" } else { "right" };
        log!("throw by hand: released (the game's throwing hand: the {game_hand}), {:.1} m/s", math::length(hand.velocity));
    }
    RELEASING.with(|r| r.set(Some(hand)));
    // SAFETY: the game's own release on the controller whose update just ran, on the thread that
    // runs it, as the update itself makes it on the release event.
    unsafe { std::mem::transmute::<usize, ReleaseFn>(release)(controller) };
    RELEASING.with(|r| r.set(None));
}

unsafe extern "C" fn velocity(controller: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call: the item's own velocity along the look.
    let result = unsafe { VELOCITY_ORIGINAL.get()(controller, out) };
    let Some(hand) = RELEASING.with(Cell::get) else { return result };
    // SAFETY: the caller's out vector, just written by the original.
    let Some(game) = (unsafe { result.as_mut() }) else { return result };
    if let Some(throw) = launch::velocity(&Leaving { thrown: Thrown::Throwable, way: Way::Given, hand, game: Some(*game) }) {
        *game = throw;
    }
    result
}

unsafe extern "C" fn start(controller: usize, out: *mut [f32; 3]) -> *mut [f32; 3] {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let result = unsafe { START_ORIGINAL.get()(controller, out) };
    let Some(hand) = RELEASING.with(Cell::get) else { return result };
    // SAFETY: the caller's out vector, just written by the original.
    let Some(point) = (unsafe { result.as_mut() }) else { return result };
    if let Some(at) = launch::start(&Leaving { thrown: Thrown::Throwable, way: Way::Given, hand, game: None }) {
        *point = at;
    }
    result
}

/// Whether `item` was thrown by the local player.
fn players_throw(item: usize, build: &WeaponBuild, player_vtable: usize) -> bool {
    if mem::read::<u8>(item + build.thrown).unwrap_or(0) == 0 {
        return false;
    }
    let Some(owner) = mem::read::<usize>(item + build.owner).filter(|&o| o > OWNER_PART) else { return false };
    mem::read::<usize>(owner - OWNER_PART) == Some(player_vtable)
}

unsafe extern "C" fn launch(item: usize, toss: u8) {
    let _flight = InFlight::enter();
    let before = WEAPON_BUILD.get().and_then(|(b, _)| mem::read::<[f32; 3]>(item + b.velocity));
    // SAFETY: forwards the game's own call.
    unsafe { LAUNCH_ORIGINAL.get()(item, toss) };
    let Some(&(build, player_vtable)) = WEAPON_BUILD.get() else { return };
    // Only a launch that just happened: the velocity was zero and now is not.
    let Some(game) = mem::read::<[f32; 3]>(item + build.velocity) else { return };
    if before != Some([0.0; 3]) || math::length(game) < 1e-3 || !players_throw(item, &build, player_vtable) {
        return;
    }
    let Some(hand) = launch::let_go(Thrown::Weapon) else { return };
    if let Some(throw) = launch::velocity(&Leaving { thrown: Thrown::Weapon, way: Way::Given, hand, game: Some(game) }) {
        mem::write(item + build.velocity, throw);
    }
}
