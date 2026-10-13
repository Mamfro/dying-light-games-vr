//! Provider: the skeleton (`monaka_body::Skeleton`: bones, rest pose, reading and writing the
//! pose, the posing moment), with the hand rig's hook; and the held item
//! (`monaka_body::held::HeldItems`: what each hand holds, noted at that hook).
//!
//! The first-person arms follow the controllers (`hand_rig`): around the arms visual's
//! camera-target callback (`engine::FPP_CAMERA_TARGET`) the shared Chrome rig
//! ([`eng_chr::providers::fpp::around`]) snapshots the arms as animated, then puts them on the
//! controllers with their tracked fingers (Monaka's hand rig, [`crate::Dl1`]'s `HandRig`) and
//! feeds the swings. While the rig poses the arms, the game's anti-wall squash for the flat screen
//! is off at its own switch (`squash`), as in Dying Light 2 and The Beast, where it is off by
//! default: no bone is pulled toward the camera, and the weapons' effects keep their full size.
//! What is Dying Light 1's own: its arms skeleton
//! is the visual's `IModelObject` (`eng_chr::providers::model`, on this build's plain element
//! exports; the rest pose read by resetting a hand to its reference frame), reached through the
//! visual's model holder ([`arms_of`]); the camera is mapped back to the game's when the present
//! thread turned it first ([`camera_matrix`]), and its aim and tracking origin are its own (`aim`,
//! `eye_site`). Hand aim points the game camera where the held weapon points
//! ([`monaka_core::math::converged`]).
//!
//! Its probes (`probe_hands`, `probe_fingers`, `probe_rig`) are in `research::arms`.

use crate::Dl1;
use crate::providers::hand_world;
use crate::providers::engine::{self, Camera, CameraTargetFn};
use crate::providers::eye_site;
use eng_chr::providers::fpp;
use eng_chr::providers::model::Model;
use monaka_body::{BoneAxes, Skeleton};
use eng_chr::providers::held::Lines;
use monaka_body::hand_rig::{self, Climbing, HandRig, Hands, Palms};
use monaka_body::hand_trail::HandTrails;
use monaka_body::hands::{self, At, In, Point};
use monaka_body::held::{HeldItems, Item, Kind, Line};
use monaka_core::camera::Mat34;
use monaka_core::protocol::{LEFT_HAND, RIGHT_HAND};
use monaka_hook::Hooks;
use monaka_hook::module::Module;
use monaka_hook::probe::class_name;
use monaka_hook::{InFlight, Original, mem};
use monaka_producer::game::{Lack, Start};
use monaka_producer::{Rejection, log};
use std::ffi::c_void;
use monaka_core::sync::Locked;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::Mutex;

/// The hand rig (Monaka's) on Dying Light 1's own bone axes (its wrist and gun holds are the ones
/// measured), the left bones' mirroring declared ([`LEFT_MIRROR`]), climbing as the game reports it
/// ([`climbing`](super::climbing)).
impl HandRig for Dl1 {
    const RIG: hand_rig::Rig = eng_chr::providers::fpp::chrome_rig(BoneAxes::DYING_LIGHT_1, Some(LEFT_MIRROR), Climbing::Reported);

    fn held(side: usize) -> Kind {
        eng_chr::providers::held::kind(side)
    }

    fn item(side: usize) -> Option<Item> {
        eng_chr::providers::held::item(side)
    }

    fn holder(side: usize) -> Option<i32> {
        eng_chr::providers::held::holder_of(side)
    }

    /// The controllers' palms in the world, which DL1 notes itself after each arms callback
    /// (`hand_world::CONTROLLERS`).
    fn trail() -> Option<&'static HandTrails> {
        Some(&hand_world::CONTROLLERS)
    }

    const NOTES_TRAIL: bool = false;

    /// The hands' elements read back after each arms callback (`hand_world::HANDS`).
    fn posed_hands() -> Option<&'static HandTrails> {
        Some(&hand_world::HANDS)
    }

    /// Between the right hand's index and middle fingers (`hand_world::string_point`).
    fn string_point() -> Option<[f32; 3]> {
        hand_world::string_point()
    }
}

/// From a gun's holder (`R_HandHolder`) to its muzzle, along the barrel (metres).
pub(crate) const MUZZLE: f32 = 0.25;

/// Dying Light 1's own lines in the hand's holder: the rig holds a gun so its holder's +z runs
/// along the barrel, the muzzle [`MUZZLE`] out; a melee weapon's blade runs along the holder's +y,
/// its edge facing +z (the tip moves that way through the game's own swings, left and right alike).
const LINES: Lines = Lines {
    gun: Some(Line { from: [0.0; 3], along: [0.0, 0.0, 1.0], length: Some(MUZZLE), edge: None }),
    melee: Some(Line { from: [0.0; 3], along: [0.0, 1.0, 0.0], length: None, edge: Some([0.0, 0.0, 1.0]) }),
};

/// What the hands hold (`eng_chr::providers::held`), with Dying Light 1's own lines ([`LINES`]; a
/// bow's from its model).
impl HeldItems for Dl1 {
    type Model = Model;

    fn line(side: usize) -> Option<Line> {
        eng_chr::providers::held::line(side, &LINES)
    }

    fn model(side: usize) -> Option<Model> {
        eng_chr::providers::held::model(side)
    }
}

/// The hand rig component's install (`crate::HAND_RIG`): the hand rig's part, the arms visual's callback
/// hooked, the hands' world functions looked up.
pub(crate) fn install_hand_rig(start: &mut Start<'_, Dl1>) -> Result<(), Lack> {
    hand_rig::install::<Dl1>(start)?;
    hook_arms(start.hooks, &start.build.engine, &start.build.gamedll)?;
    hand_world::resolve(&start.build.engine)?;
    Ok(())
}

/// The arms visual's camera-target callback (the rig, and its probes).
pub(crate) fn hook_arms(hooks: &mut Hooks, engine_module: &Module, gamedll: &Module) -> Result<(), Lack> {
    resolve(engine_module)?;
    SQUASH_MODE.store(gamedll.at(engine::ANTI_WALL_MODE), Relaxed);
    GIVEN_BACK.store(false, Relaxed);
    hooks.site(&CAMERA_TARGET_ORIGINAL, gamedll, &crate::providers::engine::FPP_CAMERA_TARGET, camera_target)?;
    Ok(())
}

/// The game's anti-wall mode ([`engine::ANTI_WALL_MODE`]) in the game DLL, once hooked.
static SQUASH_MODE: AtomicUsize = AtomicUsize::new(0);
/// The mode the game had before the rig turned the squash off, while it is off.
static SQUASH_KEPT: Mutex<Option<i32>> = Mutex::new(None);
/// The stop gave the mode back: the callback no longer turns the squash off.
static GIVEN_BACK: AtomicBool = AtomicBool::new(false);
/// Times the game turned the squash on again under the rig (a new arms visual, its cheat menu).
static SQUASH_RETURNS: AtomicU64 = AtomicU64::new(0);

/// The game's anti-wall squash: off (`on` false) while the rig poses the arms, the game's own mode
/// kept; on, that mode given back. Turning it off is asserted at every arms callback, before the
/// game's own call reads it: the game writes the mode itself.
fn squash(on: bool) {
    let at = SQUASH_MODE.load(Relaxed);
    if at == 0 {
        return;
    }
    let mut kept = SQUASH_KEPT.locked();
    if on {
        if let Some(mode) = kept.take() {
            mem::write::<i32>(at, mode);
            log!("arms: the game's anti-wall squash given back (mode {mode})");
        }
        return;
    }
    if GIVEN_BACK.load(Relaxed) {
        return;
    }
    let Some(mode) = mem::read::<i32>(at).filter(|&m| m != 0) else { return };
    if kept.is_none() {
        *kept = Some(mode);
        log!("arms: the game's anti-wall squash off while the rig poses the arms (its mode {mode})");
    } else {
        let n = SQUASH_RETURNS.fetch_add(1, Relaxed) + 1;
        if n <= 5 {
            log!("arms: the game turned its anti-wall squash on again (mode {mode}, time {n}): off again");
        }
    }
    mem::write::<i32>(at, 0);
}

/// The stop's give-back: the game's anti-wall mode as it had it.
pub fn give_back() {
    GIVEN_BACK.store(true, Relaxed);
    squash(true);
}

pub static CAMERA_TARGET_ORIGINAL: Original<CameraTargetFn> = Original::new();

const LAYOUT: fpp::Layout = fpp::Layout { weapons: engine::FPP_VIS_WEAPONS };

/// Looks up the engine's element functions (`eng_chr::providers::model`, on this build's plain
/// exports); the hook is installed only when the reads and the posing functions all exist.
pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    for name in [engine::SET_ELEMENT_WORLD, engine::ELEMENT_IS_BONE] {
        engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")))?;
    }
    eng_chr::providers::model::resolve(engine_module, &engine::MODEL).map_err(Rejection::revision)?;
    Ok(())
}

/// How the arms' left bones mirror the right ones (row-major 3x3): across their local x-z plane,
/// the same every run. Declared, so a run that starts with the arms posed unlike (a bow drawn)
/// does not go without it and put the left hand on upside down.
pub(crate) const LEFT_MIRROR: [f32; 9] = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];

/// The arms vis's vtable, once its class name has been checked.
static VIS_VTABLE: AtomicU64 = AtomicU64::new(0);

fn is_arms(vis: usize) -> bool {
    let Some(vtable) = mem::read::<usize>(vis) else { return false };
    let known = VIS_VTABLE.load(Relaxed);
    if known == 0 && class_name(vis).as_deref() == Some(engine::FPP_VIS_CLASS) {
        VIS_VTABLE.store(vtable as u64, Relaxed);
        return true;
    }
    known != 0 && known == vtable as u64
}

/// The arms model, asked from the vis's model holder the way the callback itself asks.
fn model(vis: usize) -> Option<usize> {
    let holder = vis + engine::FPP_VIS_MODEL_HOLDER;
    let get = mem::read::<usize>(mem::read::<usize>(holder)? + engine::MODEL_HOLDER_GET_MODEL)?;
    // Only code of the game DLL: anything else means the layout is not the one this code expects.
    if !Module::find(engine::GAMEDLL).is_some_and(|m| m.contains(get)) {
        return None;
    }
    // SAFETY: the holder's own virtual, `IModelObject* (FakeModelObject*)`, called on the game
    // thread right after the game's own calls to it.
    let model = unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void) -> *mut c_void>(get)(holder as *mut c_void) };
    (!model.is_null()).then_some(model as usize)
}

/// The model of what the arms hold in weapon slot `slot` (0 the right hand's, 1 the left's), if
/// anything (a weapon's visual is its own model).
pub(crate) fn weapon_model(vis: usize, slot: usize) -> Option<Model> {
    Model::of(mem::read::<usize>(vis + LAYOUT.weapons + slot * 8).filter(|&w| w != 0)?)
}

/// The model of the bow the arms hold, if they hold one (a bow's visual is its own model).
pub(crate) fn held_bow(vis: usize) -> Option<Model> {
    Model::of(fpp::held_bow(vis, &LAYOUT)?)
}

/// The vis's arms model (`eng_chr::providers::model`), or why not.
pub(crate) fn arms_of(vis: usize) -> Result<Model, String> {
    let model = model(vis).ok_or("no arms model")?;
    let model = Model::of(model).ok_or_else(|| format!("arms model {model:#x} has no element functions or no plausible element count"))?;
    ARMS_MODEL.store(model.id() as u64, Relaxed);
    Ok(model)
}

/// Calls that were handed the camera already turned to the view.
static UNTURNED: AtomicU64 = AtomicU64::new(0);

/// The camera's camera-to-world matrix as the game set it, if the interface and its state point
/// at each other. The present thread turns the player camera to the view, and once in a few
/// hundred updates that turn lands between the game setting the camera and calling the arms: the
/// arms would then be placed against the head, a one-frame jump of the gun in one eye. Such a
/// camera is mapped back to the game's ([`eye_site::game_camera`]).
pub(crate) fn camera_matrix(camera: usize) -> Option<Mat34> {
    let state = mem::read::<usize>(camera + engine::INTERFACE_STATE)?;
    (mem::read::<usize>(state + engine::STATE_INTERFACE)? == camera).then_some(())?;
    let read = Camera::read(state)?.inverse;
    let game = eye_site::game_camera(&read);
    if game != read {
        UNTURNED.fetch_add(1, Relaxed);
    }
    Some(game)
}

/// The arms model the rig last looked up (0 before the first).
static ARMS_MODEL: AtomicU64 = AtomicU64::new(0);

/// The arms model the rig last looked up, for probes comparing it with the game's own reads.
pub fn arms_model() -> usize {
    ARMS_MODEL.load(Relaxed) as usize
}

/// A player controller's (a weapon's, a bow's, a throw's, the lockpick's) getter for its player:
/// vtable slot 0x20, from the base every player controller shares (`PlayerModuleBase`).
const PLAYER_SLOT: usize = 0x20;

/// Whether `controller` is the player's: its player is the arms model the rig poses (its getter
/// in the game DLL; called on the game thread).
pub fn players(controller: usize) -> bool {
    let ours = arms_model();
    let getter = mem::read::<usize>(controller).and_then(|vtable| mem::read::<usize>(vtable + PLAYER_SLOT));
    let Some(getter) = getter.filter(|&g| monaka_hook::module::Module::find(crate::providers::engine::GAMEDLL).is_some_and(|m| m.contains(g))) else { return false };
    // SAFETY: the controller's own player getter, on the game thread.
    ours != 0 && unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(usize) -> usize>(getter)(controller) } == ours
}

/// Arms callbacks so far (the rig poses the arms once per callback).
pub fn rig_calls() -> u64 {
    fpp::calls()
}

/// `PlayerFppVis`'s camera-target callback `(vis, camera)`.
pub unsafe extern "system" fn camera_target(vis: *mut c_void, camera: *mut c_void) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET_ORIGINAL.get()(vis, camera) };
    let (vis, camera) = (vis as usize, camera as usize);
    let arms = is_arms(vis);
    if !(eye_site::capturing() && arms) {
        if arms {
            squash(true);
        }
        original();
        return;
    }
    let config = eye_site::config();
    squash(!config.aim.rig);
    let palms = [LEFT_HAND, RIGHT_HAND].map(|side| hands::pose::<crate::Dl1>(side, Point::Palm, At::Frame, In::Tracking));
    let throwing = crate::providers::throws::throwing();
    // The game says when it climbs (not the hands' height: a throw's wind-up raises them too).
    if let Some(climbing) = crate::providers::climbing::climbing(arms_model()) {
        monaka_body::traverse::report_climbing(climbing);
    }
    let options = fpp::Options {
        rig: config.aim.rig,
        probe: crate::research::options().hands,
        melee: config.melee,
        fingers: config.aim.rig && config.fingers.tracking,
        palms,
        apart: throwing,
        // The game holds what it throws in the right hand's weapon slot.
        throw_in_hand: throwing,
    };
    let game = camera_matrix(camera);
    let hands = |game: Mat34| -> Result<Hands, &'static str> {
        if !monaka_body::look::facing::live() {
            return Err("head aim not steering");
        }
        // The same tracking origin the view is drawn from, so a hand lands where its controller is seen.
        let origin = eye_site::tracking_origin(&game, true, config.aim.hand);
        Ok(Hands::Palms(Palms { game, origin, head: eye_site::pending_pose(), palms, aim_side: config.aim.side, hold: config.hand_hold, bones: None }))
    };
    let outcome = fpp::around::<crate::Dl1, _>(vis, &LAYOUT, &options, arms_of, game, original, hands);
    crate::research::hands::moved(outcome.placed.as_ref(), game);
    crate::research::hands::after(vis, camera, outcome.before.as_ref());
    // Where the viewer's left reticle sits in the world: along where the left controller points.
    let left_reticle = game.zip(palms[LEFT_HAND]).map(|(game, palm)| {
        let m = monaka_core::camera::apply_head(&eye_site::tracking_origin(&game, true, config.aim.hand), palm.orientation, palm.position);
        let reach = monaka_core::math::WEAPON_CONVERGENCE;
        [m[3] - m[2] * reach, m[7] - m[6] * reach, m[11] - m[10] * reach]
    });
    crate::extras::grapple::note_reticle(left_reticle);
    monaka_body::bow::posed::<crate::Dl1>(outcome.placed.is_some(), left_reticle);
    crate::extras::left_hand_tool::place_tool(vis);
    crate::research::bow::after(vis);
    hand_world::sample();
    if let Some(game) = game {
        hand_world::CONTROLLERS.note(&eye_site::tracking_origin(&game, true, config.aim.hand), &palms);
    }
    note_frame();
}

/// Callbacks per presented frame, by the eye that frame renders: a frame with none was drawn with
/// whatever the game's own update left in the arms.
struct FrameTally {
    present: u64,
    calls: u64,
    eye: i32,
    /// [eye][0, 1, 2, 3+ calls]
    counts: [[u64; 4]; 2],
}

static FRAMES: Mutex<FrameTally> = Mutex::new(FrameTally { present: 0, calls: 0, eye: -1, counts: [[0; 4]; 2] });

fn note_frame() {
    let Some(present) = eye_site::drawing_present() else { return };
    let Ok(mut t) = FRAMES.lock() else { return };
    if present != t.present {
        if t.present != 0 && (0..2).contains(&t.eye) {
            let eye = t.eye as usize;
            let bucket = t.calls.min(3) as usize;
            t.counts[eye][bucket] += 1;
            // Frames in between had no callback at all; their eyes alternate.
            for skipped in t.present + 1..present.min(t.present + 64) {
                let e = (eye + (skipped - t.present) as usize) % 2;
                t.counts[e][0] += 1;
            }
        }
        t.present = present;
        t.calls = 0;
        t.eye = eye_site::eye();
    }
    t.calls += 1;
}

/// End-of-run tally.
pub fn report() {
    fpp::report();
    let unturned = UNTURNED.load(Relaxed);
    if unturned > 0 {
        log!("  handed the camera already turned to the view {unturned}x (mapped back)");
    }
    if let Ok(t) = FRAMES.lock() {
        for (eye, counts) in t.counts.iter().enumerate() {
            log!("  frames for eye {eye} by arms callbacks [0, 1, 2, 3+]: {counts:?}");
        }
    }
}
