//! Provider: the bow's draw (`monaka_body::launch::Draws`) and loosing, with the bow by hand's facts
//! (`monaka_body::bow::Bows`). Its component is `crate::BOW_BY_HAND`; the arrows are placed by the
//! fire hook ([`crate::providers::projectile`]).
//!
//! The bow controller (`WeaponBowController`) says each update whether it draws ([`update`]: its
//! state drawing or drawn). It looses an arrow ([`loose`]) with a speed from how far the bow is
//! drawn (0 to 1, at +0x6c): the player's loosing sets that from the draw the bow ready-made
//! answers (`launch::draw`), and the arrow it fires asks the ready-made where it goes
//! (`projectile::Asked`, its speed kept).

use crate::Dl1;
use crate::providers::{projectile, skeleton};
use monaka_body::bow::{Bowing, Bows};
use monaka_body::launch::{self, Draws, Weapon};
use monaka_core::gesture::PullRules;
use monaka_core::sync::Locked;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem};
use monaka_producer::Rejection;
use monaka_producer::game::{Lack, Start};
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// `WeaponBowController`'s loosing of an arrow: `(controller)`, run from its update: a bullet from
/// the level's pool at the draw's speed along the bow's shot direction; its update (vtable slot 16):
/// `(controller, seconds)`.
const LOOSE: usize = 0xd9cf40;
const UPDATE: usize = 0xd9bfe0;
/// In the controller: its state (1 drawing, 3 drawn).
const STATE: usize = 0x20;
const DRAWING: [i32; 2] = [1, 3];
/// In the controller: how far it is drawn (0 to 1).
const DRAWN: usize = 0x6c;

type LooseFn = unsafe extern "C" fn(*mut c_void);
type UpdateFn = unsafe extern "C" fn(*mut c_void, f32);

static LOOSE_ORIGINAL: Original<LooseFn> = Original::new();
static UPDATE_ORIGINAL: Original<UpdateFn> = Original::new();
/// When the player's bow was seen drawing at its latest update (none: not drawing then).
static DRAWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The draw taken as the arrow is loosed; drawing as the latest update said.
impl Draws for Dl1 {
    fn install(start: &mut Start<'_, Self>) -> Result<(), Lack> {
        Ok(install(start.hooks, &start.build.gamedll)?)
    }

    fn drawn_ago() -> Option<Duration> {
        DRAWN_AT.locked().map(|at| at.elapsed())
    }
}

/// The line from the nock as the draw began, the bow turned toward the left reticle within 30
/// degrees; the string pulled 0.05 m back counts, 0.5 m is a full draw, 0.7 m the most it goes. The
/// bow model's nock is `arrow_pose`, its string `bone_string`, the bone it bends from `bone_root`.
/// The string hand is between the right hand's index and middle fingers ([`hand_world`]).
impl Bows for Dl1 {
    const BOWING: Bowing = Bowing { pull: PullRules::BOW, most_turn: 30.0, nock: "arrow_pose", nock_like: None, string: "bone_string", bow: "bone_root" };
}

/// Hooks the bow's loosing and update (once).
pub fn install(hooks: &mut Hooks, gamedll: &Module) -> Result<(), Rejection> {
    if LOOSE_ORIGINAL.is_set() {
        return Ok(());
    }
    // SAFETY: the detours have the functions' signatures (read from their code); the game DLL's
    // build is checked by the caller; the prologues are decoded and moved.
    unsafe {
        hooks.inline_decoded(&LOOSE_ORIGINAL, "bow loose", gamedll.at(LOOSE), loose as LooseFn)?;
        hooks.inline_decoded(&UPDATE_ORIGINAL, "bow update", gamedll.at(UPDATE), update as UpdateFn)?;
    }
    Ok(())
}

unsafe extern "C" fn loose(controller: *mut c_void) {
    let _flight = InFlight::enter();
    let at = controller as usize;
    let shot = skeleton::players(at).then(|| {
        if let Some(share) = launch::draw() {
            mem::write::<f32>(at + DRAWN, share);
        }
        projectile::Asked { weapon: Weapon::Bow, aim: None }
    });
    // SAFETY: forwards the game's own call.
    projectile::aim(shot, || unsafe { LOOSE_ORIGINAL.get()(controller) });
}

unsafe extern "C" fn update(controller: *mut c_void, seconds: f32) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    unsafe { UPDATE_ORIGINAL.get()(controller, seconds) };
    let at = controller as usize;
    if skeleton::players(at) {
        let drawing = mem::read::<i32>(at + STATE).is_some_and(|s| DRAWING.contains(&s));
        *DRAWN_AT.locked() = drawing.then(Instant::now);
    }
}
