//! Where the hands are in the game's world, for what the hands do with the world's objects
//! (throwing, the bow, the reload gesture): after each arms callback the rig has posed the arms
//! model on the controllers, and each hand's element (`L_Hand`, `R_Hand`) is read back with the
//! engine's element getters ([`sample`]). A short history gives each hand's velocity.

use crate::player::hands;
use monaka_hook::mem;
use monaka_hook::module::Module;
use monaka_producer::{Rejection, log};
use std::collections::VecDeque;
use std::ffi::{CStr, c_void};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ELEMENT_ID: &str = "?GetElementID@IModelObject@@QEBAHPEBD@Z";
type ElementIdFn = unsafe extern "system" fn(usize, *const i8) -> i32;

/// The hands' elements, by side (`LEFT`, `RIGHT`).
pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;
const NAMES: [&CStr; 2] = [c"L_Hand", c"R_Hand"];
/// The history kept, and the span a velocity is measured over.
const KEPT: Duration = Duration::from_millis(600);
const VELOCITY_SPAN: Duration = Duration::from_millis(70);

struct Getters {
    element_id: ElementIdFn,
    world: crate::engine::ElementWorldFn,
}

static GETTERS: OnceLock<Getters> = OnceLock::new();
/// A hand's positions, newest last.
type Track = VecDeque<(Instant, [f32; 3])>;
/// Each hand's.
static HISTORY: Mutex<[Track; 2]> = Mutex::new([VecDeque::new(), VecDeque::new()]);
/// Each controller's world positions, newest last.
static CONTROLLERS: Mutex<[Track; 2]> = Mutex::new([VecDeque::new(), VecDeque::new()]);

pub fn resolve(engine_module: &Module) -> Result<(), Rejection> {
    let find = |name: &str| engine_module.export(name).ok_or_else(|| Rejection::revision(format!("the engine does not export {name}")));
    // SAFETY: each export has the signature its mangled name states, on x64.
    let getters = unsafe {
        Getters {
            element_id: std::mem::transmute::<usize, ElementIdFn>(find(ELEMENT_ID)?),
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

/// Adds each side's position to a history (clearing a side with none), dropping what is too old.
fn keep(history: &Mutex<[Track; 2]>, positions: [Option<[f32; 3]>; 2]) {
    let now = Instant::now();
    let Ok(mut history) = history.lock() else { return };
    for (side, position) in positions.into_iter().enumerate() {
        let list = &mut history[side];
        match position.filter(|p| p.iter().all(|c| c.is_finite())) {
            Some(p) => {
                // Several callbacks in one update give the same pose: one sample each.
                if list.back().is_none_or(|(at, _)| now.duration_since(*at) > Duration::from_millis(2)) {
                    list.push_back((now, p));
                }
            }
            None => list.clear(),
        }
        while list.front().is_some_and(|(at, _)| now.duration_since(*at) > KEPT) {
            list.pop_front();
        }
    }
}

/// A hand's latest position in the world.
pub fn position(side: usize) -> Option<[f32; 3]> {
    HISTORY.lock().ok()?[side].back().map(|(_, p)| *p)
}

/// A hand's fastest velocity (m/s, each over [`VELOCITY_SPAN`]) in the last `within`: the swing
/// behind an action the game carries out a little after it.
pub fn fastest(side: usize, within: Duration) -> Option<[f32; 3]> {
    fastest_in(&HISTORY, side, within)
}

/// A controller's fastest velocity in the world (m/s) in the last `within` ([`sample_controllers`]).
pub fn controller_fastest(side: usize, within: Duration) -> Option<[f32; 3]> {
    fastest_in(&CONTROLLERS, side, within)
}

fn fastest_in(history: &Mutex<[Track; 2]>, side: usize, within: Duration) -> Option<[f32; 3]> {
    let history = history.lock().ok()?;
    let list = &history[side];
    let (newest_at, _) = *list.back()?;
    let mut best: Option<[f32; 3]> = None;
    for (i, &(at, p)) in list.iter().enumerate() {
        if newest_at.duration_since(at) > within {
            continue;
        }
        // The latest sample at least a span earlier.
        let Some(&(from_at, from)) = list.iter().take(i).rev().find(|(t, _)| at.duration_since(*t) >= VELOCITY_SPAN) else { continue };
        let dt = at.duration_since(from_at).as_secs_f32();
        let v = [0, 1, 2].map(|k| (p[k] - from[k]) / dt);
        if best.is_none_or(|b| length(v) > length(b)) {
            best = Some(v);
        }
    }
    best
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

pub fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// `m` with its 3x3 part made a rotation (the game's arm and weapon matrices carry a squash),
/// keeping its x axis and the plane of x and y.
pub fn rigid(m: &[f32; 12]) -> [f32; 12] {
    let unit = |v: [f32; 3]| {
        let l = length(v);
        if l > 1e-9 { v.map(|c| c / l) } else { v }
    };
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let x = unit([m[0], m[4], m[8]]);
    let z = unit(cross(x, [m[1], m[5], m[9]]));
    let y = cross(z, x);
    [x[0], y[0], z[0], m[3], x[1], y[1], z[1], m[7], x[2], y[2], z[2], m[11]]
}

pub fn report() {
    if GETTERS.get().is_some() {
        let held = HISTORY.lock().map(|h| [h[0].len(), h[1].len()]).unwrap_or_default();
        log!("hands in the world: samples held at the stop {held:?}");
    }
}
