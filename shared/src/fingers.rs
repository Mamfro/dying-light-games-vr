//! Tracked fingers on the Chrome Engine arms (Dying Light 1 and 2, The Beast), on Monaka's finger
//! rig ([`monaka_arms::finger_rig`]): each knuckle's curl axis comes from the skeleton's own
//! reference pose ([`ArmsSkeleton::rest_poses`]), not a per-game bone-axis rule. Each hand is read
//! once per skeleton as a small skeleton of its own (the hand, the bones its fingers hang from, the
//! fingers), and posed in the hand's own space: the arms' world matrices carry the engine's squash
//! toward the camera, which cancels there.
//!
//! Names: under `r_hand` and `l_hand` the thumb `finger01` -> `finger02` -> `finger03`, and the
//! index, middle, ring and little finger's three knuckles `finger11`-`13`, `21`-`23`, `31`-`33`,
//! `41`-`43`. Each finger hangs from its first knuckle's parent: in Dying Light 2 a metacarpal
//! `finger10` ... `40`; in Dying Light 1 the hand for the index and middle finger and a palm bone
//! `hand1` for the ring and little finger. The thumb's first bone and the bones fingers hang from
//! stay where the rig put them; the `_normal_mask` helpers under the hand are left alone.

use crate::fpp::ArmsSkeleton;
use monaka_arms::finger_rig::{Chains, Rig};
use monaka_core::camera::{self, IDENTITY_MATRIX, Mat34};
use monaka_core::protocol::{FingerPose, LEFT_HAND, RIGHT_HAND};
use monaka_core::sync::Locked;
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One hand as a skeleton of its own: its elements (the hand first, parents before children), its
/// finger rig, and the finger bones it writes back.
struct Hand {
    elements: Vec<i32>,
    rig: Rig,
    fingers: Vec<usize>,
}

/// A skeleton's hands, read once per skeleton.
struct Hands {
    skeleton: usize,
    sides: [Option<Hand>; 2],
}

static HANDS: Mutex<Option<Hands>> = Mutex::new(None);
/// Updates each hand's fingers were put on its tracking.
static PLACED: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Hand `side` of `skeleton`, by the names above.
fn hand_of<S: ArmsSkeleton>(skeleton: &S, side: usize) -> Result<Hand, String> {
    let prefix = ["l_", "r_"][side];
    let find = |name: String| skeleton.find(&name).ok_or(format!("no {name}"));
    let hand = find(format!("{prefix}hand"))?;
    let chains = (0..5).map(|finger| (1..=3).map(|n| find(format!("{prefix}finger{finger}{n}"))).collect::<Result<Vec<i32>, String>>()).collect::<Result<Vec<_>, String>>()?;
    // The hand, every bone between it and a finger, the fingers: each after its parent.
    let mut elements = vec![hand];
    for chain in &chains {
        let mut between = Vec::new();
        let mut bone = skeleton.parent(chain[0]);
        while bone != hand {
            if bone < 0 {
                return Err(format!("{prefix}finger's bones are not under {prefix}hand"));
            }
            between.push(bone);
            bone = skeleton.parent(bone);
        }
        for bone in between.into_iter().rev().chain(chain.iter().copied()) {
            if !elements.contains(&bone) {
                elements.push(bone);
            }
        }
    }
    let local = |element: i32| elements.iter().position(|&e| e == element);
    let parents: Vec<i32> = elements.iter().enumerate().map(|(i, &e)| if i == 0 { -1 } else { local(skeleton.parent(e)).map_or(-1, |p| p as i32) }).collect();
    let rests = skeleton.rest_poses(hand, &elements[1..]).ok_or("the engine gives no rest pose for the arms skeleton")?;
    let rest = elements.iter().enumerate().map(|(i, &e)| if i == 0 { Some(IDENTITY_MATRIX) } else { rests.iter().find(|(r, _)| *r == e).map(|(_, m)| *m) });
    let rest: Vec<Mat34> = rest.collect::<Option<_>>().ok_or("a finger bone has no rest pose")?;
    let mut fingers: [[Vec<usize>; 5]; 2] = Default::default();
    fingers[side] = std::array::from_fn(|finger| chains[finger].iter().filter_map(|&b| local(b)).collect());
    let rig = Rig::discover(&parents, rest, &Chains { wrists: [0, 0], fingers: fingers.clone() })?;
    Ok(Hand { elements, rig, fingers: fingers[side].iter().flatten().copied().collect() })
}

fn read<S: ArmsSkeleton>(skeleton: &S) -> Hands {
    let sides = [LEFT_HAND, RIGHT_HAND].map(|side| match hand_of(skeleton, side) {
        Ok(hand) => Some(hand),
        Err(why) => {
            log!("fingers: the {} hand stays as the game animates it: {why}", if side == LEFT_HAND { "left" } else { "right" });
            None
        }
    });
    if sides.iter().any(Option::is_some) {
        log!("fingers: the arms skeleton's hands read ({} bones on the left, {} on the right)", sides[0].as_ref().map_or(0, |h| h.elements.len()), sides[1].as_ref().map_or(0, |h| h.elements.len()));
    }
    Hands { skeleton: skeleton.id(), sides }
}

/// Puts hand `side`'s fingers on `tracked`, after the rig has placed the hand.
pub fn apply<S: ArmsSkeleton>(skeleton: &S, side: usize, tracked: &FingerPose) {
    let mut hands = HANDS.locked();
    if hands.as_ref().is_none_or(|h| h.skeleton != skeleton.id()) {
        *hands = Some(read(skeleton));
    }
    let Some(hand) = hands.as_ref().and_then(|h| h.sides.get(side)).and_then(Option::as_ref) else { return };
    let Some(world) = skeleton.world(hand.elements[0]) else { return };
    let Some(back) = camera::inverse(&world) else { return };
    let frames = hand.elements.iter().map(|&e| skeleton.world(e).map(|m| camera::compose(&back, &m))).collect::<Option<Vec<Mat34>>>();
    let Some(mut frames) = frames else { return };
    frames[0] = IDENTITY_MATRIX;
    if !hand.rig.apply(&mut frames, side, *tracked) {
        return;
    }
    for &bone in &hand.fingers {
        let matrix = camera::compose(&world, &frames[bone]);
        skeleton.set_world(hand.elements[bone], &matrix);
        monaka_arms::note_written(skeleton.id(), hand.elements[bone], &matrix);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_arms::Skeleton;
    use std::cell::RefCell;

    /// A Dying Light 2 style left hand (metacarpals, three knuckles a finger, a three-bone thumb)
    /// under a root the engine squashes, as it squashes the arms toward the camera.
    struct Arms {
        names: Vec<String>,
        parents: Vec<i32>,
        rest: Vec<Mat34>,
        worlds: RefCell<Vec<Mat34>>,
    }

    impl Arms {
        fn new() -> Self {
            let mut arms = Arms { names: Vec::new(), parents: Vec::new(), rest: Vec::new(), worlds: RefCell::new(Vec::new()) };
            let squash: Mat34 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.6, 0.0];
            let root = arms.bone("root", -1, squash);
            let hand = arms.bone("l_hand", root, camera::translation([0.0, 0.0, -0.3]));
            // Fingers run down -z from the hand, the thumb (+x) side first; each bone 3 cm on.
            for (finger, x) in [(0, 0.04), (1, 0.02), (2, 0.0), (3, -0.02), (4, -0.04)] {
                let mut parent = if finger == 0 { hand } else { arms.bone(&format!("l_finger{finger}0"), hand, camera::translation([x, 0.0, -0.03])) };
                for n in 1..=3 {
                    let offset = if finger == 0 && n == 1 { [x, -0.01, -0.02] } else { [0.0, 0.0, -0.03] };
                    parent = arms.bone(&format!("l_finger{finger}{n}"), parent, camera::translation(offset));
                }
            }
            arms
        }

        fn bone(&mut self, name: &str, parent: i32, rest: Mat34) -> i32 {
            let world = if parent < 0 { rest } else { camera::compose(&self.worlds.borrow()[parent as usize], &rest) };
            self.names.push(name.to_owned());
            self.parents.push(parent);
            self.rest.push(rest);
            self.worlds.borrow_mut().push(world);
            self.names.len() as i32 - 1
        }
    }

    impl Skeleton for Arms {
        fn id(&self) -> usize {
            1
        }
        fn count(&self) -> i32 {
            self.names.len() as i32
        }
        fn name(&self, element: i32) -> String {
            self.names[element as usize].clone()
        }
        fn parent(&self, element: i32) -> i32 {
            self.parents[element as usize]
        }
        fn world(&self, element: i32) -> Option<Mat34> {
            self.worlds.borrow().get(element as usize).copied()
        }
        fn set_world(&self, element: i32, matrix: &Mat34) {
            self.worlds.borrow_mut()[element as usize] = *matrix;
        }
        fn is_bone(&self, _: i32) -> bool {
            true
        }
    }

    impl ArmsSkeleton for Arms {
        fn rest_poses(&self, _: i32, bones: &[i32]) -> Option<Vec<(i32, Mat34)>> {
            Some(bones.iter().map(|&b| (b, self.rest[b as usize])).collect())
        }
    }

    #[test]
    fn a_squashed_hand_curls_its_tracked_finger_alone() {
        let arms = Arms::new();
        let at = |name: &str| arms.find(name).unwrap();
        let before = arms.worlds.borrow().clone();
        let mut pose = FingerPose { valid: 1, ..FingerPose::default() };
        pose.fingers[1] = [1.0, 1.0, 1.0, 0.0];
        apply(&arms, LEFT_HAND, &pose);
        let after = arms.worlds.borrow().clone();
        let moved = |name: &str| camera::distance(&before[at(name) as usize], &after[at(name) as usize]) > 1e-4;
        assert!(moved("l_finger12") && moved("l_finger13"), "the index finger curls");
        assert!(!moved("l_hand") && !moved("l_finger10") && !moved("l_finger22"), "the hand, its metacarpal and the other fingers stay");
        // The squash stays in what is written: the bone's z axis keeps the root's 0.6.
        let tip = after[at("l_finger13") as usize];
        let length = |c: usize| (0..3).map(|r| tip[r * 4 + c] * tip[r * 4 + c]).sum::<f32>().sqrt();
        assert!((0..3).map(length).any(|l| (l - 0.6).abs() < 0.05), "{tip:?}");
    }
}
