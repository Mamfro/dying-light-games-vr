//! Tracked fingers on the Chrome Engine arms (Dying Light 1 and 2, The Beast), over the shared
//! [`monaka_arms::fingers`]: the hands' finger bones by name, their rest pose from the skeleton's
//! own reference frame ([`ArmsSkeleton::rest_poses`]), read once per skeleton.
//!
//! Names: under `r_hand` and `l_hand` the thumb `finger01` -> `finger02` -> `finger03`, and the
//! index, middle, ring and little finger's three knuckles `finger11`-`13`, `21`-`23`, `31`-`33`,
//! `41`-`43`. Each finger hangs from its first knuckle's parent: in Dying Light 2 (probed
//! 2026-10-07; 390 elements) a metacarpal `finger10` ... `40`; in Dying Light 1 the hand for the
//! index and middle finger and a palm bone `hand1` for the ring and little finger (probed
//! 2026-10-07). The thumb's first bone and the bones fingers hang from stay where the rig put them;
//! the `_normal_mask` helpers under the hand are left alone.

use crate::fpp::ArmsSkeleton;
use monaka_arms::fingers::{self, Finger, FingerJoints, Hand};
use monaka_core::camera::Mat34;
use monaka_core::math::Vec3;
use monaka_core::protocol::{FingerPose, LEFT_HAND, RIGHT_HAND};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A game's finger bone axes, in a bone's own frame: `along` runs down a bone toward its tip;
/// `curl` is the axis a knuckle turns about (right-handed) to curl toward the palm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    pub along: Vec3,
    pub curl: Vec3,
}

impl Axes {
    /// Dying Light 1: bones run along +x and curl toward the palm about +y (both hands; the rest
    /// pose and a gripping hand, 2026-10-07).
    pub const DYING_LIGHT_1: Self = Self { along: [1.0, 0.0, 0.0], curl: [0.0, 1.0, 0.0] };
    /// Dying Light 2 and The Beast (the same rig convention): along +x, curling about -y, the
    /// opposite of Dying Light 1's (+y had every finger bending backwards in the headset, DL2
    /// 2026-10-07, The Beast 2026-10-08).
    pub const DYING_LIGHT_2: Self = Self { along: [1.0, 0.0, 0.0], curl: [0.0, -1.0, 0.0] };
}

/// One skeleton's hands as finger joints and their rest poses, read once per skeleton.
struct Rig {
    skeleton: usize,
    hands: [Option<Hand>; 2],
    /// (bone, its rest pose against its parent).
    rest: Vec<(i32, Mat34)>,
}

static RIG: Mutex<Option<Rig>> = Mutex::new(None);
/// Updates each hand's fingers were put on its tracking.
static PLACED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// The hand under `prefix` (`l_` or `r_`) as fingers, by the names above, with the hand's element;
/// `None` without a hand or any finger of it.
fn hand_of<S: ArmsSkeleton>(skeleton: &S, prefix: &str, axes: &Axes) -> Option<(i32, Hand)> {
    let hand = skeleton.find(&format!("{prefix}hand"))?;
    // `count` knuckles from `finger<first>`, hanging from the first one's parent.
    let chain = |first: u32, count: u32| -> Option<Finger> {
        let knuckles = (first..first + count).map(|n| skeleton.find(&format!("{prefix}finger{n:02}"))).collect::<Option<Vec<i32>>>()?;
        let base = skeleton.parent(knuckles[0]);
        (base >= 0).then_some(Finger { base, knuckles, followers: Vec::new() })
    };
    let fingers = [chain(2, 2), chain(11, 3), chain(21, 3), chain(31, 3), chain(41, 3)];
    fingers.iter().any(Option::is_some).then_some((hand, Hand { fingers, along: axes.along, curl: axes.curl }))
}

fn read<S: ArmsSkeleton>(skeleton: &S, axes: &Axes) -> Rig {
    let mut rig = Rig { skeleton: skeleton.id(), hands: [None, None], rest: Vec::new() };
    for (side, prefix) in [(LEFT_HAND, "l_"), (RIGHT_HAND, "r_")] {
        let Some((hand, fingers)) = hand_of(skeleton, prefix, axes) else { continue };
        let bones: Vec<i32> = fingers.fingers.iter().flatten().flat_map(|f| f.knuckles.iter().copied()).collect();
        let Some(rest) = skeleton.rest_poses(hand, &bones) else {
            log!("fingers: the engine gives no rest pose for the arms skeleton; fingers stay as the game animates them");
            return rig;
        };
        rig.rest.extend(rest);
        rig.hands[side] = Some(fingers);
    }
    let count = |hand: &Option<Hand>| hand.as_ref().map_or(0, |h| h.fingers.iter().flatten().count());
    if rig.hands.iter().all(Option::is_none) {
        log!("fingers: no finger bones found on the arms skeleton ({} elements); fingers stay as the game animates them", skeleton.count());
    } else {
        log!("fingers: the arms skeleton's finger bones and rest pose read: {} fingers on the left hand, {} on the right ({} bones)", count(&rig.hands[0]), count(&rig.hands[1]), rig.rest.len());
    }
    rig
}

/// The skeleton as finger joints: world matrices through the engine, rest poses as read.
struct Joints<'a, S> {
    skeleton: &'a S,
    rest: &'a [(i32, Mat34)],
}

impl<S: ArmsSkeleton> FingerJoints for Joints<'_, S> {
    fn world(&self, joint: i32) -> Option<Mat34> {
        self.skeleton.world(joint)
    }

    fn set_world(&mut self, joint: i32, matrix: &Mat34) {
        self.skeleton.set_world(joint, matrix);
        monaka_arms::note_written(self.skeleton.id(), joint, matrix);
    }

    fn rest_between(&self, from: i32, to: i32) -> Option<Mat34> {
        (self.skeleton.parent(to) == from).then(|| self.rest.iter().find(|(b, _)| *b == to).map(|(_, m)| *m)).flatten()
    }
}

/// Puts hand `side`'s fingers on `tracked`, after the rig has placed the hand.
pub fn apply<S: ArmsSkeleton>(skeleton: &S, axes: &Axes, side: usize, tracked: &FingerPose) {
    let Ok(mut rig) = RIG.lock() else { return };
    if rig.as_ref().is_none_or(|r| r.skeleton != skeleton.id()) {
        *rig = Some(read(skeleton, axes));
    }
    let Some(rig) = rig.as_ref() else { return };
    let Some(hand) = rig.hands.get(side).and_then(Option::as_ref) else { return };
    fingers::apply(&mut Joints { skeleton, rest: &rig.rest }, hand, tracked);
    if PLACED[side].fetch_add(1, Relaxed) == 0 {
        log!("fingers: the {} hand's fingers follow finger tracking", if side == LEFT_HAND { "left" } else { "right" });
    }
}

/// End-of-run line, when fingers were placed at all.
pub fn report() {
    let (left, right) = (PLACED[LEFT_HAND].load(Relaxed), PLACED[RIGHT_HAND].load(Relaxed));
    if left + right > 0 {
        log!("fingers: placed from tracking {left} times (left), {right} (right)");
    }
}
