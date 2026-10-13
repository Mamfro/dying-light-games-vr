//! Provider part: the hands in the world, the hand rig's (`monaka_body::hand_rig::HandRig`): its
//! trail ([`CONTROLLERS`], noted here at each arms callback, not by the rig), where the game's hands
//! are after the rig and its animation ([`HANDS`]) and where the right hand holds a bowstring
//! ([`string_point`]), for the throw, bow and reload ready-mades.
//!
//! Where the hands are in the game's world, for what the hands do with the world's objects
//! (throwing, the bow, the reload gesture): after each arms callback the rig has posed the arms
//! model on the controllers, and each hand's element (`L_Hand`, `R_Hand`) is read back with the
//! engine's element getters ([`sample`]). A short trail of each, and of each controller
//! ([`CONTROLLERS`]), gives its velocity (`monaka_body::hand_trail`).

use crate::providers::skeleton;
use monaka_body::hand_trail::{HandTrails, Lost, TrailRules};
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

type ElementIdFn = unsafe extern "system" fn(usize, *const i8) -> i32;

/// The hands' elements, by side (`LEFT`, `RIGHT`).
pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;
const NAMES: [&CStr; 2] = [c"L_Hand", c"R_Hand"];
/// The trail kept and the span a velocity is measured over (seconds); a side not sampled starts
/// afresh.
pub const TRAILS: TrailRules = TrailRules { kept: 0.6, span: 0.07, lost: Lost::Forgotten };

struct Getters {
    element_id: ElementIdFn,
    world: crate::providers::engine::ElementWorldFn,
}

static GETTERS: OnceLock<Getters> = OnceLock::new();
/// Each hand's element's way in the world ([`sample`]).
pub static HANDS: HandTrails = HandTrails::new(TRAILS);
/// Each controller's way in the world, noted each arms callback on the tracking origin the rig
/// puts the hands on. A throw's swing is read from these: the arm the game animates through a
/// throw lags or stays behind the controller.
pub static CONTROLLERS: HandTrails = HandTrails::new(TRAILS);

pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let getters = unsafe {
        Getters {
            element_id: std::mem::transmute::<usize, ElementIdFn>(find(crate::providers::engine::ELEMENT_ID)?),
            world: std::mem::transmute::<usize, crate::providers::engine::ElementWorldFn>(find(crate::providers::engine::ELEMENT_WORLD)?),
        }
    };
    let _ = GETTERS.set(getters);
    Ok(())
}

/// A hand element's world matrix (row-major 3x4, the translation in column 3), on the game thread.
pub fn matrix(side: usize) -> Option<[f32; 12]> {
    element(NAMES[side])
}

/// An element of the arms model by name: its world matrix, on the game thread.
pub fn element(name: &CStr) -> Option<[f32; 12]> {
    let getters = GETTERS.get()?;
    let model = skeleton::arms_model();
    if model == 0 {
        return None;
    }
    // SAFETY: the engine's own getters on the live arms model, on the game thread.
    let id = unsafe { (getters.element_id)(model, name.as_ptr()) };
    if id < 0 {
        return None;
    }
    // SAFETY: a valid element of that model; the engine returns a reference to its matrix.
    mem::read::<[f32; 12]>(unsafe { (getters.world)(model as *mut c_void, id) } as usize)
}

/// Each arms callback, after the rig: both hands' positions into their trails ([`HANDS`]).
pub fn sample() {
    if GETTERS.get().is_none() {
        return;
    }
    HANDS.note_positions([LEFT, RIGHT].map(|side| matrix(side).map(|m| [m[3], m[7], m[11]])));
}

/// Where the right hand holds a bowstring: between its index and middle fingers' middle joints
/// (else the wrist).
pub fn string_point() -> Option<[f32; 3]> {
    let joint = |name: &CStr| element(name).map(|m| [m[3], m[7], m[11]]);
    match (joint(c"r_finger12"), joint(c"r_finger22")) {
        (Some(a), Some(b)) => Some([0, 1, 2].map(|k| (a[k] + b[k]) * 0.5)),
        _ => HANDS.position(RIGHT),
    }
}

/// `m` with its 3x3 part made a rotation (the game's arm and weapon matrices carry a squash),
/// keeping its x axis and the plane of x and y.
pub fn rigid(m: &[f32; 12]) -> [f32; 12] {
    let [x, y, z] = monaka_core::camera::rigid_axes(m);
    [x[0], y[0], z[0], m[3], x[1], y[1], z[1], m[7], x[2], y[2], z[2], m[11]]
}

pub fn report() {
    if GETTERS.get().is_some() {
        log!("hands in the world: samples held at the stop {:?}", HANDS.held());
    }
}

#[cfg(test)]
mod lift_checks;
