//! The arms' probes, around the arms callback (`crate::player::hands`): `probe_hands` adds to the shared
//! probe (`eng_chr::fpp`) the callback's caller and thread, the visual's element table, the eye
//! camera and the right holder in the wrist; `probe_fingers` the hands' elements with their rest
//! poses; `probe_rig` keeps what each rig move was made from, for [`super::view::rig`].

use super::options;
use crate::engine;
use crate::player::hands::{Model, camera_matrix};
use crate::view::stereo;
use eng_chr::fpp::{self, ArmsSkeleton};
use monaka_arms::{Placed, Skeleton, Snapshot};
use monaka_core::camera::{Mat34, compose, rigid_inverse};
use monaka_hook::module::Module;
use monaka_hook::probe::{caller, class_name};
use monaka_hook::{mem, thread_id};
use monaka_producer::log;
use std::sync::Mutex;

/// What the latest rig move was made from (`probe_rig`).
#[derive(Clone, Copy)]
pub struct RigSample {
    pub palm: Mat34,
    pub origin: Mat34,
    pub game: Mat34,
    pub call: u64,
    pub present: u64,
}

static LAST_RIG: Mutex<Option<RigSample>> = Mutex::new(None);

pub fn last_rig() -> Option<RigSample> {
    LAST_RIG.lock().ok().and_then(|last| *last)
}

/// The rig moved the arms (`placed`) against the game camera `game` (`probe_rig`).
pub fn moved(placed: Option<&Placed>, game: Option<Mat34>) {
    if !options().rig {
        return;
    }
    let (Some(placed), Some(game)) = (placed, game) else { return };
    // The same tracking origin the rig used: the view's.
    let origin = stereo::tracking_origin(&game, true, stereo::config().aim.hand);
    if let Ok(mut last) = LAST_RIG.lock() {
        *last = Some(RigSample { palm: placed.palm, origin, game, call: fpp::calls(), present: stereo::drawing_present().unwrap_or(0) });
    }
}

/// After an arms callback on the visual `vis` with `camera`; `before` the arms as animated.
pub fn after(vis: usize, camera: usize, before: Option<&Snapshot>) {
    let call = fpp::calls();
    if options().hands {
        if fpp::probing(call) {
            probe(vis, camera, call);
        }
        probe_holder(vis, before);
    }
    if options().fingers && call == 300 {
        probe_fingers(vis);
    }
}

/// `probe_hands`, beside the shared probe: the callback's caller and thread, the visual's element
/// table (the elements the callback squashes for the flat screen) and the eye camera.
fn probe(vis: usize, camera: usize, call: u64) {
    let Some(game) = camera_matrix(camera) else {
        log!("hands probe {call}: camera {camera:#x} has no readable state");
        return;
    };
    let view = rigid_inverse(&game);
    let player = stereo::player_camera();
    log!(
        "hands probe {call}: from {} thread {} camera {camera:#x} {} (player camera {}), camera at [{:.3} {:.3} {:.3}]",
        Module::describe(caller()),
        thread_id(),
        class_name(camera).unwrap_or_default(),
        if player == 0 { "not known yet".to_string() } else { (player == camera).to_string() },
        game[3],
        game[7],
        game[11]
    );
    let Ok(model) = Model::of(vis) else { return };
    let table = mem::read::<usize>(vis + engine::FPP_VIS_ELEMENTS);
    for (slot, label) in engine::FPP_ELEMENT_SLOTS {
        let Some(element) = table.and_then(|t| mem::read::<i32>(t + slot)) else { continue };
        match (0..model.count()).contains(&element).then(|| model.world(element)).flatten() {
            Some(world) => log!("  {label}: element {element} {} {}", model.name(element), fpp::in_camera(&view, &world)),
            None => log!("  {label}: element {element} (none)"),
        }
    }
    if let Some(eye) = model.find("eyecamera").and_then(|e| model.world(e)) {
        log!("  eyecamera {}", fpp::in_camera(&view, &eye));
    }
}

/// `probe_hands`: the right holder in the right wrist as animated, half a second after the weapon
/// visual changes (once the draw animation has mostly settled) and every 5 s, for the hold
/// calibration.
fn probe_holder(vis: usize, before: Option<&Snapshot>) {
    static LAST: Mutex<(usize, u64)> = Mutex::new((0, 0));
    let Ok(mut last) = LAST.lock() else { return };
    let weapon = mem::read::<usize>(vis + engine::FPP_VIS_WEAPONS).unwrap_or(0);
    if weapon != last.0 {
        *last = (weapon, 0);
    }
    last.1 += 1;
    if !(last.1 == 30 || last.1.is_multiple_of(300)) {
        return;
    }
    let (Ok(model), Some(before)) = (Model::of(vis), before) else { return };
    let (Some(holder), Some(wrist)) = (model.find("r_handholder"), model.find("r_hand")) else { return };
    let (Some(&h), Some(&w)) = (before.worlds.get(holder as usize), before.worlds.get(wrist as usize)) else { return };
    log!("  holder in wrist {}", fpp::rows(&compose(&rigid_inverse(&w), &h)));
}

/// `probe_fingers`: each hand's elements under it, with the reference frame (rest pose): names,
/// parents, rest pose against the parent, and how the animated pose turns from it.
fn probe_fingers(vis: usize) {
    let Ok(model) = Model::of(vis) else { return };
    for hand_name in ["l_hand", "r_hand"] {
        let Some(hand) = model.find(hand_name) else {
            log!("fingers probe: no element {hand_name}");
            continue;
        };
        let under = model.under(hand);
        let animated: Vec<(i32, Option<Mat34>)> = under.iter().map(|&e| (e, model.local(e))).collect();
        let Some(rest) = model.at_rest(hand, &under, |m| under.iter().map(|&e| (e, m.local(e))).collect::<Vec<_>>()) else {
            log!("fingers probe: the engine lacks the reset or local matrix exports");
            return;
        };
        let of = |list: &[(i32, Option<Mat34>)], e: i32| list.iter().find(|(x, _)| *x == e).and_then(|(_, w)| *w);
        log!("fingers probe: {hand_name} (element {hand}) has {} elements under it", under.len() - 1);
        for &e in under.iter().skip(1) {
            let rest_local = of(&rest, e);
            // The animated turn against the rest pose, in the element's own frame (axis, degrees).
            let turn = rest_local.zip(of(&animated, e)).map(|(r, a)| {
                let d = compose(&rigid_inverse(&r), &a);
                let angle = ((d[0] + d[5] + d[10] - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees();
                let axis = [d[9] - d[6], d[2] - d[8], d[4] - d[1]];
                let l = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt().max(1e-6);
                (axis.map(|v| v / l), angle)
            });
            log!(
                "  {e} '{}' parent '{}' bone {}: rest translation {:.3?} rows {:.2?}; animated turn {:.2?}",
                model.name(e),
                model.name(model.parent(e)),
                model.is_bone(e),
                rest_local.map(|m| [m[3], m[7], m[11]]),
                rest_local.map(|m| [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]]),
                turn
            );
        }
    }
}
