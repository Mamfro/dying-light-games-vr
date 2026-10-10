//! The first-person arms visual's camera-target callback in the Chrome Engine games: the player
//! camera update calls it right after setting the camera, the moment the arms are placed for the
//! frame. A producer hooks it and calls [`around`] with the game's own call: the arms are snapshot
//! before it, then put on the controllers ([`monaka_arms::follow`]) and melee swings fed
//! ([`monaka_arms::melee`]). Games differ in addresses ([`Layout`]), in how they reach the arms
//! skeleton (an [`ArmsSkeleton`]: Dying Light 2 and The Beast through the `CoSkeleton` component,
//! Dying Light 1 through `IModelObject`'s element exports) and in how they know their camera and
//! view (the `pose` closure).

use monaka_arms::melee::{self, Held};
use monaka_arms::{Placed, Pose, Skeleton, Snapshot};
use monaka_core::camera::{Mat34, compose, rigid_inverse};
use monaka_core::protocol::HandPose;
use monaka_hook::module::Module;
use monaka_hook::{Hooks, Instruction, Original, mem};
use monaka_hook::probe::class_name;
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// A Chrome Engine arms skeleton: what the rig, the fingers and the probe need beyond
/// [`Skeleton`].
pub trait ArmsSkeleton: Skeleton {
    /// The element called `name`, if any (a linear search: for reading a layout once).
    fn find(&self, name: &str) -> Option<i32> {
        (0..self.count()).find(|&e| self.name(e) == name)
    }

    /// The rest poses (each against its parent: the skeleton's reference frame) of `bones`, all
    /// under the element `hand`; a bone the engine gives none for is left out. `None` when the
    /// engine gives no rest poses at all.
    fn rest_poses(&self, hand: i32, bones: &[i32]) -> Option<Vec<(i32, Mat34)>>;

    /// What kind of element `element` is, for the probe.
    fn kind_label(&self, element: i32) -> String {
        match (self.is_bone(element), self.parent(element) >= 0) {
            (true, _) => "bone",
            (false, true) => "helper",
            (false, false) => "mesh part",
        }
        .to_string()
    }
}

/// The arms visual's camera-target callback: `(its ICameraTarget base, the camera)`.
pub type CameraTargetFn = unsafe extern "system" fn(target: usize, camera: usize);

/// Where a game's arms camera-target callback is: its RVA in the game DLL with its first
/// instructions, and the target vtable (an RVA) whose `slot` must name it.
#[derive(Clone, Copy, Debug)]
pub struct Callback {
    pub rva: usize,
    pub prologue: &'static [Instruction],
    pub vtable: usize,
    pub slot: usize,
}

/// Hooks the callback in `gamedll` with `detour` (the game's original into `original`), once the
/// vtable slot is seen to name it.
///
/// # Safety
/// `gamedll` is the build `callback` describes.
pub unsafe fn hook(hooks: &mut Hooks, gamedll: &Module, callback: &Callback, original: &'static Original<CameraTargetFn>, detour: CameraTargetFn) -> Result<(), Rejection> {
    let target = gamedll.at(callback.rva);
    if mem::read::<usize>(gamedll.at(callback.vtable) + callback.slot) != Some(target) {
        return Err(Rejection::revision("arms camera target: the vis vtable slot does not name it"));
    }
    // SAFETY: the exact prologue is checked by the hooker; the detour has the callback's type.
    Ok(unsafe { hooks.inline(original, "arms camera target", target, callback.prologue, detour) }?)
}

/// Where a game keeps the arms visual's weapons: the first of two weapon visual pointers.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub weapons: usize,
}

/// What one call does.
pub struct Options {
    /// Put the arms on the controllers and swing to attack.
    pub rig: bool,
    /// Log the skeleton's elements and the weapon visuals (`probe_hands`).
    pub probe: bool,
    pub melee: melee::Settings,
    /// With the rig: a free hand's fingers follow the headset's finger tracking, on these bone
    /// axes ([`crate::fingers`]); `None` leaves the fingers to the game.
    pub fingers: Option<crate::fingers::Axes>,
    /// Each controller's palm in tracking space, by side, for the swings (the same the `pose`
    /// closure puts the arms on).
    pub palms: [Option<HandPose>; 2],
}

/// What one call did: the arms as animated (taken when the rig or the probe wanted them) and,
/// when the rig moved them, where.
pub struct Outcome {
    pub before: Option<Snapshot>,
    pub placed: Option<Placed>,
}

struct Counters {
    calls: AtomicU64,
    moved: AtomicU64,
    not_moved: AtomicU64,
}
static COUNTERS: Counters = Counters { calls: AtomicU64::new(0), moved: AtomicU64::new(0), not_moved: AtomicU64::new(0) };
static SKIPS: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

/// Calls of [`around`] so far.
pub fn calls() -> u64 {
    COUNTERS.calls.load(Relaxed)
}

/// Whether call `call` is one the probe logs.
pub fn probing(call: u64) -> bool {
    call == 60 || call.is_multiple_of(600)
}

/// Around the game's own camera-target call (`original`) for the arms visual `vis`: snapshot,
/// call, then the rig. `skeleton` reaches the arms skeleton from the visual (or says why not);
/// `camera` is the player camera as the game set it, if known; `pose` builds what the rig puts
/// the arms on from it and whether a gun is drawn (`Err` with why to leave the arms to the game
/// this update).
pub fn around<S: ArmsSkeleton>(
    vis: usize,
    layout: &Layout,
    options: &Options,
    skeleton: impl FnOnce(usize) -> Result<S, String>,
    camera: Option<Mat34>,
    original: impl FnOnce(),
    pose: impl FnOnce(Mat34, bool) -> Result<Pose, &'static str>,
) -> Outcome {
    let call = COUNTERS.calls.fetch_add(1, Relaxed) + 1;
    let probing = options.probe && probing(call);
    let skeleton = if options.rig || probing { skeleton(vis).map_err(|why| note_lookup(Some(why))).ok() } else { None };
    let before = skeleton.as_ref().and_then(|s| {
        let snapshot = Snapshot::take(s);
        // Unreadable elements are left alone (`Snapshot`); noted for the log.
        let unreadable = snapshot.as_ref().and_then(|snap| snap.worlds.iter().position(|w| !w[0].is_finite())).map(|e| e as i32);
        note_lookup(match (&snapshot, unreadable) {
            (None, _) => Some("no element readable".into()),
            (Some(_), Some(element)) => Some(format!("element {element} ({}) of {} has no finite world matrix; it is left alone", s.name(element), s.count())),
            (Some(_), None) => None,
        });
        snapshot
    });
    original();
    if probing && let (Some(skeleton), Some(camera)) = (&skeleton, camera) {
        probe(skeleton, before.as_ref(), &camera, vis, call);
    }
    if options.probe {
        probe_weapons(vis, layout);
    }
    if !options.rig {
        return Outcome { before, placed: None };
    }
    let weapon = mem::read::<usize>(vis + layout.weapons).unwrap_or(0);
    let held = Held::of(weapon);
    melee::update(held, options.palms, &options.melee);
    let moved = match (&skeleton, &before, camera) {
        (Some(skeleton), Some(before), Some(camera)) => pose(camera, held == Held::Gun).and_then(|pose| monaka_arms::follow(skeleton, before, &pose)),
        (_, _, None) => Err("no player camera"),
        _ => Err("no skeleton"),
    };
    (if moved.is_ok() { &COUNTERS.moved } else { &COUNTERS.not_moved }).fetch_add(1, Relaxed);
    match moved {
        Err(why) => {
            note_skip(why, call);
            Outcome { before, placed: None }
        }
        Ok(placed) => {
            if let (Some(axes), Some(skeleton)) = (options.fingers, &skeleton) {
                monaka_arms::fingers::place_free_hands(options.melee.aim_side, weapon != 0, |side, tracked| crate::fingers::apply(skeleton, &axes, side, tracked));
            }
            Outcome { before, placed: Some(placed) }
        }
    }
}

fn note_skip(why: &'static str, call: u64) {
    let Ok(mut skips) = SKIPS.lock() else { return };
    let total = match skips.iter_mut().find(|(w, _)| *w == why) {
        Some((_, n)) => {
            *n += 1;
            *n
        }
        None => {
            skips.push((why, 1));
            1
        }
    };
    if call > 300 && total <= 5 {
        log!("hand rig: call {call} left the arms to the game: {why}");
    }
}

/// Logs why the skeleton could not be read (or that parts of it cannot) whenever the reason
/// changes, the first few times.
pub fn note_lookup(why: Option<String>) {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    let Ok(mut last) = LAST.lock() else { return };
    if *last != why {
        if LOGGED.fetch_add(1, Relaxed) < 20 {
            match &why {
                Some(why) => log!("hand rig: arms skeleton: {why}"),
                None => log!("hand rig: arms skeleton readable again"),
            }
        }
        *last = why;
    }
}

/// Where `world` sits in the space of the camera whose inverse is `view`, and how long its axes
/// are (1 when unscaled).
pub fn in_camera(view: &Mat34, world: &Mat34) -> String {
    let local = compose(view, world);
    let length = |c: usize| (local[c] * local[c] + local[4 + c] * local[4 + c] + local[8 + c] * local[8 + c]).sqrt();
    format!("at [{:.3} {:.3} {:.3}] axes {:.2} {:.2} {:.2}", local[3], local[7], local[11], length(0), length(1), length(2))
}

/// A 3x4 matrix's rows, for the log.
pub fn rows(m: &Mat34) -> String {
    format!(
        "[{:.4} {:.4} {:.4} {:.4} | {:.4} {:.4} {:.4} {:.4} | {:.4} {:.4} {:.4} {:.4}]",
        m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]
    )
}

/// The skeleton's elements against the camera (all of them on the first probe, the arm joints
/// later), with the wrists' and the hand holders' animated frames (rows of the 3x4, for comparing
/// skeletons' bone axes: `BoneAxes`, and for the hold calibration).
fn probe<S: ArmsSkeleton>(skeleton: &S, before: Option<&Snapshot>, game: &Mat34, vis: usize, call: u64) {
    let view = rigid_inverse(game);
    let describe = |element: i32| -> String {
        let after = skeleton.world(element).map_or_else(|| "unreadable".to_string(), |w| in_camera(&view, &w));
        match before.and_then(|b| b.worlds.get(element as usize)) {
            Some(world) if skeleton.world(element).as_ref() != Some(world) => format!("{after}; before the callback {}", in_camera(&view, world)),
            _ => after,
        }
    };
    log!(
        "hands probe {call}: vis {vis:#x}, skeleton {:#x} {} with {} elements (camera space: x right, y up, -z ahead)",
        skeleton.id(),
        class_name(skeleton.id()).unwrap_or_default(),
        skeleton.count()
    );
    let all = call == 60;
    for element in 0..skeleton.count() {
        let name = skeleton.name(element);
        let lower = name.to_ascii_lowercase();
        if all || ["hand", "upperarm", "forearm", "clavicle"].iter().any(|j| lower.ends_with(j)) {
            log!("  [{element}] {name} {} parent {} {}", skeleton.kind_label(element), skeleton.parent(element), describe(element));
        }
        if ["r_hand", "l_hand", "r_handholder", "l_handholder"].contains(&lower.as_str())
            && let Some(world) = before.and_then(|b| b.worlds.get(element as usize))
        {
            log!("  {lower}: animated frame {}", rows(&compose(&view, world)));
        }
    }
}

/// The arms visual's two weapon visuals, logged whenever either changes, with their classes.
fn probe_weapons(vis: usize, layout: &Layout) {
    static LAST: Mutex<[usize; 2]> = Mutex::new([0; 2]);
    let Ok(mut last) = LAST.lock() else { return };
    let now = [mem::read::<usize>(vis + layout.weapons).unwrap_or(0), mem::read::<usize>(vis + layout.weapons + 8).unwrap_or(0)];
    if now != *last {
        let describe = |p: usize| if p == 0 { "none".to_string() } else { format!("{p:#x} {}", class_name(p).unwrap_or_default()) };
        log!("weapons: slot 0 {}, slot 1 {}", describe(now[0]), describe(now[1]));
        *last = now;
    }
}

/// End-of-run tally (with the rig's own).
pub fn report() {
    let c = &COUNTERS;
    let calls = c.calls.load(Relaxed);
    if calls > 0 {
        log!("arms callback: {calls} calls; moved to the controller {}, left to the game {}", c.moved.load(Relaxed), c.not_moved.load(Relaxed));
    }
    if let Ok(skips) = SKIPS.lock() {
        for (why, n) in skips.iter() {
            log!("  left to the game {n}x: {why}");
        }
    }
    crate::fingers::report();
    monaka_arms::report();
}
