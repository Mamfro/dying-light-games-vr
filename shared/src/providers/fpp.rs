//! Provider: the skeleton's posing moment (the arms visual's callback), where the hand rig poses
//! the arms, and the arms' facts the rig reads there.
//!
//! The first-person arms visual's camera-target callback in the Chrome Engine games: the player
//! camera update calls it right after setting the camera, the moment the arms are placed for the
//! frame. A producer hooks it and calls [`around`] with the game's own call: the arms are snapshot
//! before it, then put on the controllers with their fingers by Monaka's hand rig
//! ([`monaka_arms::hand_rig`], [`chrome_rig`]) and melee swings fed ([`monaka_arms::melee`]).
//! Games differ in addresses ([`Layout`]), in how they reach the arms skeleton (an
//! [`ArmsSkeleton`]: Dying Light 2 and The Beast through the `CoSkeleton` component, Dying Light 1
//! through `IModelObject`'s element exports) and in how they know their camera and view (the
//! `hands` closure).

use monaka_arms::finger_rig::{ChainSource, FingerNames};
use monaka_arms::hand_rig::{self, Climbing, Hands, Holds, Method, Moment, Rig};
use monaka_arms::held::HeldItems;
use monaka_arms::melee::{self, Held};
use monaka_arms::{BoneAxes, Placed, Skeleton, Snapshot};
use monaka_core::camera::{Mat34, compose, rigid_inverse};
use monaka_core::protocol::{HandPose, LEFT_HAND, RIGHT_HAND};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, Instruction, Original, mem};
use monaka_hook::probe::class_name;
use monaka_producer::{Rejection, log};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// A gun's visual class (all three games); melee weapons are its base, `WeaponVis`.
pub const GUN_VIS_CLASS: &str = ".?AVFireWeaponVis@@";
/// A throwable's visual class: a molotov's bottle.
pub const THROWABLE_VIS_CLASS: &str = ".?AVMolotovVis@@";

/// Whether the visual `weapon` (0 for none) is of class `class`: by its vtable (`known`) once one
/// of the class has been named.
fn is_class(weapon: usize, known: &AtomicU64, class: &str) -> bool {
    let Some(vtable) = mem::read::<usize>(weapon).filter(|_| weapon != 0) else { return false };
    let seen = known.load(Relaxed);
    if seen != 0 {
        return seen == vtable as u64;
    }
    let of = class_name(weapon).as_deref() == Some(class);
    if of {
        known.store(vtable as u64, Relaxed);
    }
    of
}

/// Whether the weapon visual `weapon` is a gun (`FireWeaponVis`, by its vtable once named).
pub fn is_gun(weapon: usize) -> bool {
    static GUN_VTABLE: AtomicU64 = AtomicU64::new(0);
    is_class(weapon, &GUN_VTABLE, GUN_VIS_CLASS)
}

/// Whether the weapon visual `weapon` is a throwable's ([`THROWABLE_VIS_CLASS`], by its vtable once
/// named).
pub fn is_throwable(weapon: usize) -> bool {
    static THROWABLE_VTABLE: AtomicU64 = AtomicU64::new(0);
    is_class(weapon, &THROWABLE_VTABLE, THROWABLE_VIS_CLASS)
}

/// What the arms' weapon visual `weapon` (0 for none) is, for the shared melee: a gun
/// ([`is_gun`]), another weapon, or nothing.
pub fn held_of(weapon: usize) -> Held {
    match weapon {
        0 => Held::Nothing,
        w if is_gun(w) => Held::Gun,
        _ => Held::Melee,
    }
}

/// The fingers of every Chrome Engine arms skeleton: under `r_hand` and `l_hand` the thumb
/// `finger01` -> `finger02` -> `finger03`, and the index, middle, ring and little finger's three
/// knuckles `finger11`-`13`, `21`-`23`, `31`-`33`, `41`-`43`. Each finger hangs from its first
/// knuckle's parent: in Dying Light 2 a metacarpal `finger10` ... `40`; in Dying Light 1 the hand
/// for the index and middle finger and a palm bone `hand1` for the ring and little finger. The
/// thumb's first bone and the bones fingers hang from stay where the rig put them.
pub const FINGERS: FingerNames = FingerNames {
    wrists: ["l_hand", "r_hand"],
    sides: ["l_", "r_"],
    fingers: ["finger0", "finger1", "finger2", "finger3", "finger4"],
    bone: "{side}{finger}{n}",
};

/// The hand rig of a Chrome Engine game: the arms carried under the head on its bone axes
/// `bones`, a gun or a rifle's front hand held as the game holds it, fingers by [`FINGERS`].
pub const fn chrome_rig(bones: BoneAxes, mirror: Option<[f32; 9]>, climbing: Climbing) -> Rig {
    Rig { method: Method::Carried { bones, mirror, melee_holder: None }, holds: Holds::TwoHanded, climbing, fingers: Some(ChainSource::Named(FINGERS)) }
}

/// A Chrome Engine arms skeleton: what the rig and the probe need beyond [`Skeleton`].
pub trait ArmsSkeleton: Skeleton {
    /// The element called `name`, if any (a linear search: for reading a layout once).
    fn find(&self, name: &str) -> Option<i32> {
        (0..self.count()).find(|&e| self.name(e) == name)
    }

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
    /// With the rig: a free hand's fingers follow the headset's finger tracking.
    pub fingers: bool,
    /// Each controller's palm in tracking space, by side, for the swings (the same the `hands`
    /// closure puts the arms on).
    pub palms: [Option<HandPose>; 2],
    /// The game's animation brings the hands together now but each stays on its own controller (a
    /// tool thrown from the left hand). A bow always is ([`is_bow`]).
    pub apart: bool,
    /// The aim hand's weapon slot holds what a throw under way throws (Dying Light 1 holds the tool
    /// there for its throw): what it holds is a throwable ([`crate::providers::held::reported`]).
    pub throw_in_hand: bool,
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
/// When the rig last saw the arms hold a bow.
static BOW_SEEN: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// A bow seen this recently is still held.
const BOW_FRESH: std::time::Duration = std::time::Duration::from_millis(300);

/// Calls of [`around`] so far.
pub fn calls() -> u64 {
    COUNTERS.calls.load(Relaxed)
}

/// Whether call `call` is one the probe logs.
pub fn probing(call: u64) -> bool {
    call == 60 || call.is_multiple_of(600)
}

/// Around the game's own camera-target call (`original`) for the arms visual `vis`: snapshot, call,
/// what the arms hold noted ([`crate::providers::held`]), then game `G`'s hand rig. `skeleton`
/// reaches the arms skeleton from the visual (or says why not); `camera` is the player camera as
/// the game set it, if known; `hands` builds what the rig puts the arms on from it (`Err` with why
/// to leave the arms to the game this update).
pub fn around<G: HeldItems, S: ArmsSkeleton>(
    vis: usize,
    layout: &Layout,
    options: &Options,
    skeleton: impl FnOnce(usize) -> Result<S, String>,
    camera: Option<Mat34>,
    original: impl FnOnce(),
    hands: impl FnOnce(Mat34) -> Result<Hands, &'static str>,
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
    if options.probe || options.rig {
        note_weapons(vis, layout);
    }
    if !options.rig {
        return Outcome { before, placed: None };
    }
    let weapon = mem::read::<usize>(vis + layout.weapons).unwrap_or(0);
    let held = held_of(weapon);
    let bow_vis = held_bow(vis, layout);
    let bow = bow_vis.is_some();
    let holders = skeleton.as_ref().map_or([None; 2], |s| [LEFT_HAND, RIGHT_HAND].map(|side| crate::providers::held::holder(s, side)));
    // The held item says a throwable where the game does; the swings below take it as before.
    let kind = crate::providers::held::reported(held, held == Held::Melee && is_throwable(weapon), options.throw_in_hand);
    crate::providers::held::note(crate::providers::held::Noted::of(options.melee.aim_side, weapon, kind, bow_vis, holders, options.apart));
    let swung = LAST_WEAPON.lock().map(|mut last| swung_as(held, &mut last)).unwrap_or(held);
    if let Ok(mut seen) = BOW_SEEN.lock() {
        *seen = bow.then(std::time::Instant::now);
    }
    // A bow (in the left hand's slot, the right's empty) swings nothing: its draw is the attack,
    // and an empty right hand would punch, firing it.
    // A throw by hand under way: the left hand throws, it does not punch (the game's punch would
    // swing the view).
    let mut swinging = options.palms;
    if options.apart {
        swinging[monaka_core::protocol::LEFT_HAND] = None;
    }
    melee::update(if bow { Held::Gun } else { swung }, swinging, &options.melee);
    // A bow is held apart (the bow in the left hand, the string in the right), and whatever the
    // producer says is.
    monaka_arms::hands_apart(options.apart || bow);
    let moved = match (&skeleton, &before, camera) {
        (Some(skeleton), Some(before), Some(camera)) => hands(camera).and_then(|hands| hand_rig::frame::<G>(skeleton, before, Moment { hands, fingers: options.fingers })),
        (_, _, None) => Err("no player camera"),
        _ => Err("no skeleton"),
    };
    (if moved.is_ok() { &COUNTERS.moved } else { &COUNTERS.not_moved }).fetch_add(1, Relaxed);
    match moved {
        Err(why) => {
            note_skip(why, call);
            Outcome { before, placed: None }
        }
        Ok(placed) => Outcome { before, placed: Some(placed) },
    }
}

/// The last weapon the arms held (not nothing), for [`swung_as`].
static LAST_WEAPON: Mutex<Held> = Mutex::new(Held::Nothing);

/// What the swings take the arms to hold, given what they hold (`held`) and the last weapon they
/// held (`last`, kept up to date here): hands left empty after a gun (the game puts it away for a
/// climb or a vault, and back a few seconds later) still hold the gun, so an empty hand's swing
/// does not press the attack, which would draw the gun and fire it. Another weapon ends it.
fn swung_as(held: Held, last: &mut Held) -> Held {
    if held != Held::Nothing {
        *last = held;
        return held;
    }
    if *last == Held::Gun { Held::Gun } else { held }
}

/// Whether the rig saw the arms hold a bow lately.
pub fn bow_held() -> bool {
    BOW_SEEN.lock().ok().and_then(|s| *s).is_some_and(|at| at.elapsed() < BOW_FRESH)
}

/// Whether the aim should stay the head's now: the hands aim a throw under way or a held bow
/// themselves (with the bow by hand on), and the game's aim (the flashlight with it) following a
/// controller then only gets in the way.
pub fn head_aims() -> bool {
    crate::providers::throws::throwing() || (monaka_arms::bow::installed() && bow_held())
}

/// The class of the arms' weapon visual for a bow.
const BOW_VIS_CLASS: &str = ".?AVBowVis@@";

/// The bow the arms hold, in either weapon slot (a bow is held in the left hand).
pub fn held_bow(vis: usize, layout: &Layout) -> Option<usize> {
    [0, 8].into_iter().filter_map(|slot| mem::read::<usize>(vis + layout.weapons + slot)).find(|&w| is_bow(w))
}

/// Whether the arms' weapon visual `weapon` (0 for none) is a bow (its class, the vtable then kept).
pub fn is_bow(weapon: usize) -> bool {
    static BOW_VTABLE: AtomicU64 = AtomicU64::new(0);
    let first = BOW_VTABLE.load(Relaxed) == 0;
    let bow = is_class(weapon, &BOW_VTABLE, BOW_VIS_CLASS);
    if bow && first {
        log!("hand rig: a bow is held apart, the bow on the left controller and the string on the right");
    }
    bow
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
/// Logs what the arms hold in each weapon slot when it changes (the first changes).
fn note_weapons(vis: usize, layout: &Layout) {
    static LAST: Mutex<[usize; 2]> = Mutex::new([0; 2]);
    static LOGGED: AtomicU64 = AtomicU64::new(0);
    let Ok(mut last) = LAST.lock() else { return };
    let now = [mem::read::<usize>(vis + layout.weapons).unwrap_or(0), mem::read::<usize>(vis + layout.weapons + 8).unwrap_or(0)];
    if now != *last && LOGGED.fetch_add(1, Relaxed) < 40 {
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
    monaka_arms::report();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hands_after_a_gun_still_hold_it_until_another_weapon() {
        let mut last = Held::Nothing;
        assert_eq!(swung_as(Held::Nothing, &mut last), Held::Nothing, "bare hands from the start punch");
        assert_eq!(swung_as(Held::Gun, &mut last), Held::Gun);
        assert_eq!(swung_as(Held::Nothing, &mut last), Held::Gun, "the gun put away for a climb");
        assert_eq!(swung_as(Held::Melee, &mut last), Held::Melee);
        assert_eq!(swung_as(Held::Nothing, &mut last), Held::Nothing, "after a melee weapon, empty hands punch");
    }
}
