//! Where the hands are in the game's world, for what the hands do with the world's objects
//! (throwing, the bow, the reload gesture): after each arms callback the rig has posed the arms
//! model on the controllers, and each hand's element (`L_Hand`, `R_Hand`) is read back with the
//! engine's element getters ([`sample`]). A short trail of each (`monaka_core::trail`) gives its
//! velocity.

use monaka_core::math::{cross, unit};
use crate::player::hands;
use monaka_core::trail::Trail;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::ffi::{CStr, c_void};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type ElementIdFn = unsafe extern "system" fn(usize, *const i8) -> i32;

/// The hands' elements, by side (`LEFT`, `RIGHT`).
pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;
const NAMES: [&CStr; 2] = [c"L_Hand", c"R_Hand"];
/// The trail kept, and the span a velocity is measured over (seconds).
const KEPT: f64 = 0.6;
const VELOCITY_SPAN: f64 = 0.07;

struct Getters {
    element_id: ElementIdFn,
    world: crate::engine::ElementWorldFn,
}

static GETTERS: OnceLock<Getters> = OnceLock::new();
/// Each hand's element's way in the world.
static HISTORY: Mutex<[Trail; 2]> = Mutex::new([Trail::new(KEPT), Trail::new(KEPT)]);
/// Each controller's way in the world.
static CONTROLLERS: Mutex<[Trail; 2]> = Mutex::new([Trail::new(KEPT), Trail::new(KEPT)]);
/// The trails' clock.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Seconds on the trails' clock, which the hand gestures ([`crate::player::controls`]) keep time by
/// too.
pub fn now() -> f64 {
    STARTED.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let getters = unsafe {
        Getters {
            element_id: std::mem::transmute::<usize, ElementIdFn>(find(crate::engine::ELEMENT_ID)?),
            world: std::mem::transmute::<usize, crate::engine::ElementWorldFn>(find(crate::engine::ELEMENT_WORLD)?),
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
    let model = hands::arms_model();
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

/// Each arms callback, after the rig: both hands' positions into the history.
pub fn sample() {
    if GETTERS.get().is_none() {
        return;
    }
    let positions = [LEFT, RIGHT].map(|side| matrix(side).map(|m| [m[3], m[7], m[11]]));
    keep(&HISTORY, positions);
}

/// Each arms callback: where the controllers are in the world (the tracking origin the rig puts
/// the hands on, `origin`). A throw's swing is read from these: the arm the game animates through
/// a throw lags or stays behind the controller.
pub fn sample_controllers(origin: &[f32; 12], palms: [Option<monaka_core::protocol::HandPose>; 2]) {
    let positions = palms.map(|palm| {
        palm.filter(|p| p.valid != 0).map(|p| {
            let m = monaka_core::camera::apply_head(origin, p.orientation, p.position);
            [m[3], m[7], m[11]]
        })
    });
    keep(&CONTROLLERS, positions);
}

/// Adds each side's position to its trail (clearing a side with none), dropping what is too old.
/// Several callbacks in one update give the same pose: one sample each ([`Trail::SAME_UPDATE`]).
fn keep(history: &Mutex<[Trail; 2]>, positions: [Option<[f32; 3]>; 2]) {
    let now = now();
    let Ok(mut history) = history.lock() else { return };
    for (trail, position) in history.iter_mut().zip(positions) {
        match position.filter(|p| p.iter().all(|c| c.is_finite())) {
            Some(p) => {
                trail.push_position(now, p);
            }
            None => trail.clear(),
        }
        trail.expire(now);
    }
}

/// A hand's latest position in the world.
pub fn position(side: usize) -> Option<[f32; 3]> {
    HISTORY.lock().ok()?[side].latest().map(|s| s.position)
}

/// A hand's fastest velocity (m/s, each over [`VELOCITY_SPAN`]) in the last `within`: the swing
/// behind an action the game carries out a little after it.
pub fn fastest(side: usize, within: Duration) -> Option<[f32; 3]> {
    HISTORY.lock().ok()?[side].fastest(within.as_secs_f64(), VELOCITY_SPAN).map(|peak| peak.velocity)
}

/// Where a controller is in the world, as of the latest arms callback ([`sample_controllers`]).
pub fn controller(side: usize) -> Option<[f32; 3]> {
    CONTROLLERS.lock().ok()?[side].latest().map(|s| s.position)
}

/// A controller's fastest velocity in the world (m/s) in the last `within` ([`sample_controllers`]).
pub fn controller_fastest(side: usize, within: Duration) -> Option<[f32; 3]> {
    CONTROLLERS.lock().ok()?[side].fastest(within.as_secs_f64(), VELOCITY_SPAN).map(|peak| peak.velocity)
}

/// Which way a controller travelled through its fastest moment in the last `within` (unit): from
/// `before` that moment to `after` it (the newest sample at most). `None` when it hardly moved.
pub fn controller_stroke(side: usize, within: Duration, before: Duration, after: Duration) -> Option<[f32; 3]> {
    CONTROLLERS.lock().ok()?[side].stroke(within.as_secs_f64(), VELOCITY_SPAN, before.as_secs_f64(), after.as_secs_f64())
}

/// Where the right hand holds a bowstring: between its index and middle fingers' middle joints
/// (else the wrist).
pub fn string_point() -> Option<[f32; 3]> {
    let joint = |name: &CStr| element(name).map(|m| [m[3], m[7], m[11]]);
    match (joint(c"r_finger12"), joint(c"r_finger22")) {
        (Some(a), Some(b)) => Some([0, 1, 2].map(|k| (a[k] + b[k]) * 0.5)),
        _ => position(RIGHT),
    }
}

/// `m` with its 3x3 part made a rotation (the game's arm and weapon matrices carry a squash),
/// keeping its x axis and the plane of x and y.
pub fn rigid(m: &[f32; 12]) -> [f32; 12] {
    let unit = |v: [f32; 3]| unit(v).unwrap_or(v);
    let x = unit([m[0], m[4], m[8]]);
    let z = unit(cross(x, [m[1], m[5], m[9]]));
    let y = cross(z, x);
    [x[0], y[0], z[0], m[3], x[1], y[1], z[1], m[7], x[2], y[2], z[2], m[11]]
}

pub fn report() {
    if GETTERS.get().is_some() {
        let held = HISTORY.lock().map(|h| h.each_ref().map(Trail::len)).unwrap_or_default();
        log!("hands in the world: samples held at the stop {held:?}");
    }
}
