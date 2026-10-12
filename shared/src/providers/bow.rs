//! Provider part: the bow's fire hook (`monaka_arms::launch::Fires`) and draw
//! (`monaka_arms::launch::Draws`), with the bow by hand's facts ([`BOWING`], [`KEEPS`],
//! `monaka_arms::bow::Bows`), what Dying Light 2 and The Beast share.
//!
//! The bow controller (`WeaponBowController`) draws in one state: each update it works the power
//! out from the time drawn over the full draw time `t`, as the most power times `1 - (1 - t)^2`.
//! Here ([`update`]), after each update of the player's bow in that state, the power is set by the
//! same curve ([`power`]) from the draw the bow ready-made answers (`launch::draw`, asked before
//! the update, and kept as the bow looses; where it answers none, the game's own power stands); the
//! game's own timing of the draw is left as it is.
//! Each arrow is made by one call ([`arrow`]), which takes its start from the player's eye point and
//! its aim from the look: during that call both come from the ready-made's answer for the shot
//! (`launch::shot`, asked with the game's own eye and look, [`crate::providers::aim::asking`]).

use crate::feature::Chrome;
use crate::providers::aim;
use monaka_arms::bow::{Bowing, Keeps};
use monaka_arms::launch::{self, Fired, Flight, Weapon};
use monaka_core::gesture::PullRules;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::game::{Lack, Missing, Start};
use monaka_producer::{Rejection, log};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One build's bow controller code and fields (game DLL RVAs and offsets).
#[derive(Clone, Copy, Debug)]
pub struct Build {
    /// The controller's update `(controller, dt)`.
    pub update: usize,
    /// One arrow `(controller, target or 0)`.
    pub arrow: usize,
    /// The controller's state (i32); [`DRAWING`] is the draw, [`LOOSING`] the arrow let go.
    pub state: usize,
    /// The draw's power (f32), the most power right after it.
    pub power: usize,
    /// The player character's vtable (`PlayerDI_PH`).
    pub player_vtable: usize,
}

/// The draw state, and the one it goes to as the arrow is let go.
const DRAWING: i32 = 3;
const LOOSING: i32 = 4;

/// The bow by hand on the Chrome games (a game's `Bows::BOWING`), on Dying Light 1's rules: the
/// line from the nock as the draw began, the bow turned toward the left reticle within 30 degrees;
/// the string pulled 0.05 m back counts, 0.5 m is a full draw, 0.7 m the most it goes. The game
/// takes the draw at each of the bow's updates while it draws ([`update`]). The bow models' nock is
/// `arrow_pose` (else anything named for the arrow), the string `bone_string`, the bone the bow
/// bends from `bone_root`.
pub const BOWING: Bowing = Bowing { pull: PullRules::BOW, most_turn: 30.0, nock: "arrow_pose", nock_like: Some("arrow"), string: "bone_string", bow: "bone_root" };

/// How the Chrome games keep the bow model (a game's `Bows::KEEPS`): the engine works its world
/// matrices out from the locals after the arms' posing (a local matrix set marks the model for
/// that), and the bow hangs from the left hand's holder as the hand rig placed it only after, so
/// what is placed on it goes against each element's parent.
pub const KEEPS: Keeps = Keeps::Local;

type UpdateFn = unsafe extern "C" fn(usize, f32);
type ArrowFn = unsafe extern "C" fn(usize, usize);

static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
static ARROW_ORIGINAL: Original<ArrowFn> = Original::new();
/// The build, and the player vtable relocated.
static BUILD: OnceLock<(Build, usize)> = OnceLock::new();
/// When the player's bow was last seen drawing.
static DRAWN_AT: Mutex<Option<Instant>> = Mutex::new(None);
static ARROWS: AtomicU64 = AtomicU64::new(0);

/// The bow's build and the aim part's, where game `G`'s build has been read for them.
fn builds<G: Chrome>() -> Result<(Build, aim::Build), Lack> {
    G::FACTS.hands.and_then(|h| h.bow.map(|bow| (bow, h.aim))).ok_or_else(|| Lack::Missing("the bow is not read for this build".into()))
}

/// Game `G`'s draw hooked (its `Draws::install`): the bow's update.
pub fn install_draw_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let (build, _) = builds::<G>()?;
    let gamedll = crate::feature::gamedll(start)?;
    install_update(start.hooks, gamedll, build).missing()
}

/// Game `G`'s fire hook (its `Fires::install`): the bow's arrow, aimed through the aim part.
pub fn install_fire_for<G: Chrome>(start: &mut Start<'_, G>) -> Result<(), Lack> {
    let (build, aim_build) = builds::<G>()?;
    let gamedll = crate::feature::gamedll(start)?;
    aim::install(start.hooks, gamedll, aim_build).missing()?;
    install_arrow(start.hooks, gamedll, build).missing()
}

fn install_update(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if UPDATE_ORIGINAL.is_set() {
        return Ok(());
    }
    let _ = BUILD.set((build, gamedll.at(build.player_vtable)));
    // SAFETY: the detour has the update's signature (as its call sites use it); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&UPDATE_ORIGINAL, "bow update", gamedll.at(build.update), update as UpdateFn)? };
    Ok(())
}

fn install_arrow(hooks: &mut Hooks, gamedll: &Module, build: Build) -> Result<(), Rejection> {
    if ARROW_ORIGINAL.is_set() {
        return Ok(());
    }
    let _ = BUILD.set((build, gamedll.at(build.player_vtable)));
    // SAFETY: the detour has the arrow's signature (as its call sites use it); the game DLL's
    // build is checked by the caller; the prologue is decoded and moved.
    unsafe { hooks.inline_decoded(&ARROW_ORIGINAL, "bow arrow", gamedll.at(build.arrow), arrow as ArrowFn)? };
    Ok(())
}

/// How long ago the player's bow was last seen drawing (a game's `Draws::drawn_ago`).
pub fn drawn_ago() -> Option<Duration> {
    DRAWN_AT.locked().map(|at| at.elapsed())
}

/// The game's draw curve: the power for a draw's share (0 to 1) of a bow whose most power is
/// `most`.
pub fn power(share: f32, most: f32) -> f32 {
    let left = 1.0 - share.clamp(0.0, 1.0);
    most * (1.0 - left * left)
}

/// The player's bow controller's build, where `controller` is the player's.
fn players(controller: usize) -> Option<Build> {
    BUILD.get().filter(|&&(_, player_vtable)| crate::providers::throws::players(controller, player_vtable)).map(|&(build, _)| build)
}

unsafe extern "C" fn update(controller: usize, dt: f32) {
    let _flight = InFlight::enter();
    let players = players(controller);
    let state = players.and_then(|build| mem::read::<i32>(controller + build.state));
    let share = if state == Some(DRAWING) {
        *DRAWN_AT.locked() = Some(Instant::now());
        launch::draw()
    } else {
        None
    };
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, dt) };
    // The power from the draw (this update's, as the bow looses too), by the game's own curve.
    if let (Some(build), Some(share)) = (players, share)
        && matches!(mem::read::<i32>(controller + build.state), Some(DRAWING | LOOSING))
        && let Some(most) = mem::read::<f32>(controller + build.power + 4).filter(|m| m.is_finite())
    {
        mem::write(controller + build.power, power(share, most));
    }
}

/// The ready-made's eye and look for the player's arrow, from the game's own.
fn answer(eye: [f32; 3], look: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
    let shot = launch::shot(&Fired { weapon: Weapon::Bow, game: Flight { from: eye, velocity: look }, aim: Some(look) })?;
    ARROWS.fetch_add(1, Relaxed);
    Some((shot.from, shot.direction))
}

unsafe extern "C" fn arrow(controller: usize, target: usize) {
    let _flight = InFlight::enter();
    if players(controller).is_none() {
        // SAFETY: forwards the game's own call.
        return unsafe { ARROW_ORIGINAL.get()(controller, target) };
    }
    // SAFETY: forwards the game's own call, with the hands' aim on this thread.
    aim::asking(answer, || unsafe { ARROW_ORIGINAL.get()(controller, target) })
}

/// The hooks' counts (a game's `Bows::report`).
pub fn report() {
    if BUILD.get().is_some() {
        log!("bow by hand: {} arrows aimed by the hands ({} eye points taken from them)", ARROWS.load(Relaxed), aim::overridden());
    }
}

#[cfg(test)]
mod tests;
