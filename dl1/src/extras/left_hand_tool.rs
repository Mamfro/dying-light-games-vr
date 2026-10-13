//! The tool shown in the hand that throws it. Dying Light 1 holds a throwable in the right hand's
//! weapon slot for its throw, while the hands throw it with the left (`monaka_body::throw`): while
//! a throw runs (`crate::providers::throws::throwing`) the tool goes into the left hand, held as
//! the right holds it, mirrored ([`place_tool`], after the rig placed the arms).

use crate::providers::{hand_world, throws};
use monaka_body::Skeleton;
use monaka_core::camera::{Mat34, compose, rigid_inverse};
use monaka_core::math::length;
use monaka_producer::log;
use std::sync::atomic::{AtomicU64, Ordering::*};

/// The left hand's bones mirror the right's across their local x-z plane.
const MIRROR: Mat34 = [1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
/// A tool mirrored into the left hand flips one of its own axes back (a mirror image of it cannot
/// be drawn): across its y for most, across its z for a bottle (its y runs along it, and flipping
/// that would hold it upside down).
const TOOL_FLIP: Mat34 = MIRROR;
const BOTTLE_FLIP: Mat34 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0];
const BOTTLE_CLASS: &str = ".?AVMolotovVis@@";
/// A tool this near the right hand's holder is where the game put it this update (else it is still
/// where it was moved to, and moving it again would carry it off).
const AT_HOLDER: f32 = 0.05;

static TOOL_PLACED: AtomicU64 = AtomicU64::new(0);

/// After the rig placed the arms: while a throw runs, the tool (in the right hand's weapon slot)
/// in the left hand instead, held as the right holds it, mirrored.
pub fn place_tool(vis: usize) {
    if !throws::installed() || !throws::throwing() {
        return;
    }
    let Some(tool) = crate::providers::skeleton::weapon_model(vis, 0) else { return };
    let (Some(left), Some(right), Some(holder), Some(world)) = (hand_world::element(c"L_Hand"), hand_world::element(c"R_Hand"), hand_world::element(c"R_HandHolder"), tool.world(0)) else { return };
    let at = |m: &Mat34| [m[3], m[7], m[11]];
    let apart = length([0, 1, 2].map(|k| at(&world)[k] - at(&holder)[k]));
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
    let flip = if monaka_hook::probe::class_name(tool.id()).as_deref() == Some(BOTTLE_CLASS) { BOTTLE_FLIP } else { TOOL_FLIP };
    let target = compose(&left, &compose(&MIRROR, &compose(&in_hand, &flip)));
    let moved = compose(&compose(&target, &rigid_inverse(&holder)), &world);
    tool.set_world(0, &moved);
    let n = TOOL_PLACED.fetch_add(1, Relaxed) + 1;
    if n == 1 {
        log!("throwing by hand: the tool shown in the left hand while it is thrown");
    }
}
