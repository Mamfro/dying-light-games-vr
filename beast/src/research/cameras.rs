//! `probe=1`: observing probe of the renderer's camera (`crate::view::cameras`): which cameras are set or
//! rebuilt, from where, on which thread and how often, to find the player's camera and the one
//! place per frame where an eye camera can be written. Changes nothing: every detour counts its call
//! and runs the original with the game's own arguments (`CCamera::SetView(mtx34)` is hooked for
//! this probe alone). Camera values are read only by the reporting thread, with fault-free reads,
//! never inside a game call.

use super::options;
use crate::engine::{self, SetMatrixFn};
use monaka_hook::module::Module;
use monaka_hook::{Hooks, InFlight, Original, mem, probe};
use monaka_producer::{Rejection, log};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};

static SET_MATRIX_ORIGINAL: Original<SetMatrixFn> = Original::new();

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
enum Kind {
    SetMatrix = 1,
    SetVectors = 2,
    ComputeFrustum = 3,
}

impl Kind {
    fn from(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::SetMatrix),
            2 => Some(Self::SetVectors),
            3 => Some(Self::ComputeFrustum),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::SetMatrix => "SetView(mtx)",
            Self::SetVectors => "SetView(vec)",
            Self::ComputeFrustum => "Frustum",
        }
    }
}

/// One (kind, camera, caller) seen by a detour.
struct Site {
    key: AtomicU64,
    kind: AtomicU32,
    camera: AtomicUsize,
    caller: AtomicUsize,
    thread: AtomicU32,
    count: AtomicU64,
    reported: AtomicU64,
}

impl Site {
    const fn new() -> Self {
        Self {
            key: AtomicU64::new(0),
            kind: AtomicU32::new(0),
            camera: AtomicUsize::new(0),
            caller: AtomicUsize::new(0),
            thread: AtomicU32::new(0),
            count: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }
}

const SLOTS: usize = 256;
static SITES: [Site; SLOTS] = [const { Site::new() }; SLOTS];
static DROPPED: AtomicU64 = AtomicU64::new(0);

fn key(kind: Kind, camera: usize, caller: usize) -> u64 {
    let mut h = (camera as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (caller as u64).rotate_left(29).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    h ^= kind as u64;
    h ^= h >> 31;
    h | 1
}

/// Callers inside the renderer, whose cameras are short-lived copies (many on the stack), are
/// counted per call site rather than per camera. Such a site reports the latest camera seen
/// there, which may be gone by then (the report reads it without trusting it).
static RENDERER_RANGE: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

fn merge_cameras_called_from(module: &Module) {
    RENDERER_RANGE[0].store(module.base(), Relaxed);
    RENDERER_RANGE[1].store(module.base() + module.size(), Relaxed);
}

/// Counts one call (lock-free; a full table drops the call and counts that instead).
fn note(kind: Kind, camera: usize, caller: usize) {
    if !options().cameras {
        return;
    }
    let merged = (RENDERER_RANGE[0].load(Relaxed)..RENDERER_RANGE[1].load(Relaxed)).contains(&caller);
    let key = key(kind, if merged { 0 } else { camera }, caller);
    let start = (key as usize) % SLOTS;
    for i in 0..SLOTS {
        let site = &SITES[(start + i) % SLOTS];
        let current = site.key.load(Acquire);
        let mine = current == key
            || (current == 0
                && match site.key.compare_exchange(0, key, Acquire, Acquire) {
                    Ok(_) => {
                        site.camera.store(camera, Relaxed);
                        site.caller.store(caller, Relaxed);
                        site.thread.store(monaka_hook::thread_id(), Relaxed);
                        site.kind.store(kind as u32, Release);
                        true
                    }
                    Err(other) => other == key,
                });
        if mine {
            if merged {
                site.camera.store(camera, Relaxed);
            }
            site.count.fetch_add(1, Relaxed);
            return;
        }
    }
    DROPPED.fetch_add(1, Relaxed);
}

/// `CCamera::SetView(vec3)` on `camera`, called from `caller`.
pub fn set_vectors(camera: usize, caller: usize) {
    note(Kind::SetVectors, camera, caller);
}

/// `CCamera::ComputeFrustumMatrix` on `camera`, called from `caller`.
pub fn frustum(camera: usize, caller: usize) {
    note(Kind::ComputeFrustum, camera, caller);
}

monaka_hook::caller_shim!(set_matrix_stub => set_matrix);

unsafe extern "system" fn set_matrix(camera: usize, matrix: usize, flag: usize, caller: usize) {
    let _flight = InFlight::enter();
    note(Kind::SetMatrix, camera, caller);
    // SAFETY: the original, with the game's arguments unchanged.
    unsafe { SET_MATRIX_ORIGINAL.get()(camera, matrix, flag) }
}

/// The shimmed detour as the hooked function's type.
fn set_matrix_detour() -> SetMatrixFn {
    // SAFETY: three integer arguments, and the stub's detour takes them followed by the caller.
    unsafe { probe::as_detour(set_matrix_stub) }
}

/// Hooks `CCamera::SetView(mtx34)` (for this probe alone) and counts renderer callers per site.
///
/// # Safety
/// `renderer` is the fingerprinted renderer.
pub unsafe fn install(hooks: &mut Hooks, renderer: &Module) -> Result<(), Rejection> {
    merge_cameras_called_from(renderer);
    // SAFETY: the named export (checked) of type SetMatrixFn, its prologue checked byte for byte
    // before anything is written.
    unsafe {
        hooks.inline(
            &SET_MATRIX_ORIGINAL,
            "CCamera::SetView(mtx34)",
            crate::view::cameras::export(renderer, engine::SET_VIEW_MATRIX)?,
            engine::SET_VIEW_MATRIX.2,
            set_matrix_detour(),
        )?;
    }
    Ok(())
}

/// What a camera holds now, read without trusting the pointer.
fn describe_camera(camera: usize) -> String {
    let class = probe::class_name(camera).unwrap_or_else(|| "?".to_owned());
    let Some(inverse) = mem::read::<[f32; 12]>(camera + engine::CAMERA_INVERSE) else {
        return format!("{class} (unreadable)");
    };
    let projection = mem::read::<[f32; 16]>(camera + engine::CAMERA_PROJECTION).unwrap_or([0.0; 16]);
    let near = mem::read::<f32>(camera + engine::CAMERA_NEAR).unwrap_or(0.0);
    let far = mem::read::<f32>(camera + engine::CAMERA_FAR).unwrap_or(0.0);
    let fov = |scale: f32| if scale > 0.0 { 2.0 * (1.0 / scale).atan().to_degrees() } else { 0.0 };
    // Rows of the camera-to-world 3x4: the third column is the camera's z axis in the world.
    format!(
        "{class} pos ({:.2}, {:.2}, {:.2}) z ({:.2}, {:.2}, {:.2}) fov {:.1}x{:.1} near {near:.3} far {far:.0}",
        inverse[3],
        inverse[7],
        inverse[11],
        inverse[2],
        inverse[6],
        inverse[10],
        fov(projection[0]),
        fov(projection[5]),
    )
}

/// Logs the sites called since the last report as calls per second, busiest first within each
/// kind; `None` logs every site's total calls instead.
pub fn report(seconds: Option<f64>) {
    let everything = seconds.is_none();
    let mut rows = Vec::new();
    for site in &SITES {
        if site.key.load(Acquire) == 0 {
            continue;
        }
        let Some(kind) = Kind::from(site.kind.load(Acquire)) else { continue };
        let count = site.count.load(Relaxed);
        let since = if everything { count } else { count - site.reported.swap(count, Relaxed) };
        if since > 0 {
            rows.push((kind, since, site));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    match seconds {
        Some(seconds) => log!("--- {} sites over {seconds:.1} s, {} calls dropped", rows.len(), DROPPED.load(Relaxed)),
        None => log!("--- totals: {} sites, {} calls dropped", rows.len(), DROPPED.load(Relaxed)),
    }
    for (kind, since, site) in rows.iter().take(60) {
        let camera = site.camera.load(Relaxed);
        log!(
            "{:13} {:9.1}{} t{:<6} {:42} cam {camera:#x} {}",
            kind.name(),
            *since as f64 / seconds.unwrap_or(1.0),
            if everything { " calls" } else { "/s" },
            site.thread.load(Relaxed),
            Module::describe(site.caller.load(Relaxed)),
            describe_camera(camera),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_never_empty_and_tell_kinds_apart() {
        assert_ne!(key(Kind::SetMatrix, 0, 0), 0);
        assert_ne!(key(Kind::SetMatrix, 0x1000, 0x2000), key(Kind::ComputeFrustum, 0x1000, 0x2000));
    }
}
