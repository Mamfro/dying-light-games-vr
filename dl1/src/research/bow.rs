//! `probe_bow`: the bow model's elements, for lining its string and arrow up with the hands. The
//! first time the arms hold a bow it logs every element of the bow's model (name, parent, bone or
//! not, where it is against the bow's root, how far from each hand). Then, while the bow is held,
//! the string and the arrow's nock when they move against the root: where they are, and how far
//! from the right hand.

use crate::player::hand_world;
use monaka_arms::Skeleton;
use monaka_core::camera::{Mat34, compose, rigid_inverse};
use monaka_producer::log;
use std::sync::Mutex;

/// A move against the root this far (metres) is logged.
const MOVED: f32 = 0.01;
/// Every this many arms callbacks, the moves are looked at again.
const EVERY: u64 = 30;
/// Lines of moves logged at most.
const MOST_LINES: u32 = 300;

struct State {
    /// The bow model seen, and each element's place against the root when first seen.
    model: usize,
    first: Vec<[f32; 3]>,
    calls: u64,
    lines: u32,
}

static STATE: Mutex<State> = Mutex::new(State { model: 0, first: Vec::new(), calls: 0, lines: 0 });

fn translation(m: &Mat34) -> [f32; 3] {
    [m[3], m[7], m[11]]
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    hand_world::length([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}

/// After the rig placed the arms: the held weapon's model when it is a bow.
pub fn after(vis: usize) {
    if !super::options().bow {
        return;
    }
    let Some(model) = crate::player::hands::held_bow(vis) else { return };
    let Some(root) = model.world(0) else { return };
    let to_root = rigid_inverse(&root);
    let count = model.count();
    let against_root: Vec<[f32; 3]> = (0..count).map(|e| model.world(e).map_or([f32::NAN; 3], |m| translation(&compose(&to_root, &m)))).collect();
    let hands = [hand_world::LEFT, hand_world::RIGHT].map(|side| hand_world::matrix(side).map(|m| translation(&m)));
    let Ok(mut state) = STATE.lock() else { return };
    if state.model != model.id() {
        state.model = model.id();
        state.first = against_root.clone();
        log!("probe_bow: bow model {:#x}, {count} elements (places against element 0, the root)", model.id());
        // The bow's axes against the left hand's, for holding it in the palm.
        log!("probe_bow: root world {root:.3?}");
        for name in [c"L_Hand", c"L_HandHolder", c"l_handholder", c"R_HandHolder"] {
            log!("probe_bow: {name:?} world {:.3?}", hand_world::element(name));
        }
        for e in 0..count {
            let world = model.world(e).map(|m| translation(&m));
            let to_hand = |side: usize| match (world, hands[side]) {
                (Some(w), Some(h)) => format!("{:.3}", distance(w, h)),
                _ => "?".into(),
            };
            log!(
                "probe_bow:   {e} {:?} parent {} {} at {:.3?}, {} m from the left hand, {} from the right",
                model.name(e),
                model.parent(e),
                if model.is_bone(e) { "bone" } else { "not a bone" },
                against_root[e as usize],
                to_hand(0),
                to_hand(1)
            );
        }
        return;
    }
    state.calls += 1;
    if state.calls % EVERY != 0 || state.lines >= MOST_LINES {
        return;
    }
    for e in (0..count).filter(|&e| matches!(model.name(e).as_str(), "bone_string" | "arrow_pose")) {
        let (now, then) = (against_root[e as usize], state.first.get(e as usize).copied().unwrap_or([f32::NAN; 3]));
        let moved = distance(now, then);
        if moved > MOVED && state.lines < MOST_LINES {
            state.lines += 1;
            let right = model.world(e).zip(hands[1]).map(|(m, h)| distance(translation(&m), h));
            log!("probe_bow: call {}: {:?} moved {moved:.3} m against the root, at {now:.3?}, {:.3?} m from the right hand", state.calls, model.name(e), right);
        }
    }
}
