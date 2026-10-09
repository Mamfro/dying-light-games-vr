//! The renderer camera's exported setters, always hooked: the player camera update passes
//! `CCamera::SetView(vec3)`, where the eye camera (`stereo`), head aim (`aim`) and the turn check
//! (`camera_update`) take over, and `CCamera::ComputeFrustumMatrix`, where the headset's field of view is
//! written (`stereo::before_frustum`).
//!
//! Its probe (`probe`, which also hooks `SetView(mtx34)`) is in `research::cameras`.

use crate::engine::{self, ComputeFrustumFn, SetVectorsFn};
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original, mem, probe};
use monaka_producer::Rejection;

pub static SET_VECTORS_ORIGINAL: Original<SetVectorsFn> = Original::new();
pub static COMPUTE_FRUSTUM_ORIGINAL: Original<ComputeFrustumFn> = Original::new();

/// The address of `target`'s export in `renderer`, checked against the RVA it was inspected at.
pub(crate) fn export(renderer: &Module, (export, rva, _): engine::Target) -> Result<usize, Rejection> {
    let address = renderer.at(rva);
    match renderer.export(export) {
        Some(found) if found == address => Ok(address),
        found => Err(Rejection::revision(format!("{export} is at {found:?}, not {address:#x}"))),
    }
}

/// Four arguments, so no shim: the caller comes from a stack walk. The player camera update's
/// call may pass on an eye camera (`stereo`) or turned vectors (`camera_update`).
pub unsafe extern "system" fn set_vectors(camera: usize, forward: usize, up: usize, position: usize) {
    let _flight = InFlight::enter();
    let caller = probe::caller();
    crate::research::cameras::set_vectors(camera, caller);
    let player = crate::view::camera_update::is_player_update(caller);
    // Head aim first, from the game's own camera (the eye camera replaces it below).
    let baked = if player
        && let (Some(back), Some(game_up), Some(at)) = (mem::read::<[f32; 3]>(forward), mem::read::<[f32; 3]>(up), mem::read::<[f32; 3]>(position))
    {
        crate::player::aim::note_camera(camera, back, game_up, at)
    } else {
        None
    };
    if player
        && let Some([back, up, position]) = crate::view::stereo::eye_vectors(camera, forward, up, position, baked)
    {
        // SAFETY: the original, with the game's camera and our own vectors, which live until it
        // returns.
        unsafe { SET_VECTORS_ORIGINAL.get()(camera, back.address(), up.address(), position.address()) }
    } else if let Some((forward, up)) = crate::view::camera_update::turned(caller, forward, up) {
        // SAFETY: the original, with the game's camera and position and our own vectors, which
        // live until it returns.
        unsafe { SET_VECTORS_ORIGINAL.get()(camera, forward.address(), up.address(), position) }
    } else {
        // SAFETY: the original, with the game's arguments unchanged.
        unsafe { SET_VECTORS_ORIGINAL.get()(camera, forward, up, position) }
    }
}

monaka_hook::caller_shim!(compute_frustum_stub => compute_frustum);

unsafe extern "system" fn compute_frustum(camera: usize, flag: usize, _: usize, caller: usize) {
    let _flight = InFlight::enter();
    crate::research::cameras::frustum(camera, caller);
    crate::view::stereo::before_frustum(camera);
    // SAFETY: the original, with the game's arguments unchanged.
    unsafe { COMPUTE_FRUSTUM_ORIGINAL.get()(camera, flag) }
}

/// The shimmed detour as the hooked function's type.
pub fn compute_frustum_detour() -> ComputeFrustumFn {
    // SAFETY: two integer arguments (padded to three), followed by the caller.
    unsafe { probe::as_detour(compute_frustum_stub) }
}
