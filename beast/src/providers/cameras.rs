//! Provider, to be: part of the eye site (reading the game camera and writing an eye into it), with
//! [`eye_site`] and [`camera_update`].
//!
//! The renderer camera's exported setters, always hooked: the player camera update passes
//! `CCamera::SetView(vec3)`, where the eye camera ([`eye_site`]), head aim (`aim`) and the turn
//! check ([`camera_update`]) take over, and `CCamera::ComputeFrustumMatrix`, where the headset's
//! field of view is written ([`eye_site::before_frustum`]).
//!
//! Its probe (`probe`, which also hooks `SetView(mtx34)`) is in `research::cameras`.

use crate::providers::engine::{self, ComputeFrustumFn, SetVectorsFn};
use crate::providers::{camera_update, eye_site};
use crate::{Beast, Facts};
use monaka_core::options::Options;
use monaka_hook::{InFlight, Original, probe};
use monaka_producer::game::{Lack, Start};

pub static SET_VECTORS_ORIGINAL: Original<SetVectorsFn> = Original::new();
pub static COMPUTE_FRUSTUM_ORIGINAL: Original<ComputeFrustumFn> = Original::new();


/// Four arguments, so no shim: the caller comes from a stack walk. The player camera update's
/// call may pass on an eye camera ([`eye_site`]) or turned vectors ([`camera_update`]).
pub unsafe extern "system" fn set_vectors(camera: usize, forward: usize, up: usize, position: usize) {
    let _flight = InFlight::enter();
    let caller = probe::caller();
    crate::research::cameras::set_vectors(camera, caller);
    // The player camera update: the game's own camera noted for the arms (keyed by this renderer
    // camera) before the eye camera replaces it.
    let player = crate::providers::look_fields::AIM.player_update(camera_update::player_update(), caller, camera, forward, up, position);
    // The game's own pitch for the pitch lock, while head aim does not steer (steering, it notes
    // the character's own).
    if let Some(pitch) = player.and_then(|update| update.pitch()).filter(|_| !crate::providers::look_fields::AIM.live()) {
        crate::providers::look_fields::AIM.note_camera_pitch(pitch);
    }
    if let Some(update) = player
        && let Some([back, up, position]) = eye_site::eye_vectors(camera, &update)
    {
        // SAFETY: the original, with the game's camera and our own vectors, which live until it
        // returns.
        unsafe { SET_VECTORS_ORIGINAL.get()(camera, back.address(), up.address(), position.address()) }
    } else if let Some((forward, up)) = camera_update::turned(caller, forward, up) {
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
    eye_site::before_frustum(camera);
    // SAFETY: the original, with the game's arguments unchanged.
    unsafe { COMPUTE_FRUSTUM_ORIGINAL.get()(camera, flag) }
}

/// The shimmed detour as the hooked function's type.
pub fn compute_frustum_detour() -> ComputeFrustumFn {
    // SAFETY: two integer arguments (padded to three), followed by the caller.
    unsafe { probe::as_detour(compute_frustum_stub) }
}

/// `turn_yaw=<degrees>`: the player camera turned, the PC check that writing it reaches the
/// picture ([`camera_update`]).
pub(crate) fn turn_yaw(options: &Options) -> Option<f32> {
    options.angle("turn_yaw", 90.0).filter(|&yaw| yaw != 0.0)
}

/// The cameras component's install (`crate::CAMERAS`): the renderer camera's exported setters
/// hooked, where the eye camera, head aim and the turn check take over ([`set_vectors`]) and the
/// headset's field of view goes in ([`compute_frustum`]).
pub(crate) fn install_cameras(start: &mut Start<'_, Beast>) -> Result<(), Lack> {
    let Facts { renderer, gamedll, .. } = start.build;
    if let Some(gamedll) = gamedll {
        camera_update::set_player_update(gamedll.at(engine::PLAYER_CAMERA_UPDATE));
    }
    start.hooks.site(&SET_VECTORS_ORIGINAL, renderer, &engine::SET_VIEW_VECTORS, set_vectors)?;
    start.hooks.site(&COMPUTE_FRUSTUM_ORIGINAL, renderer, &engine::COMPUTE_FRUSTUM, compute_frustum_detour())?;
    if let Some(yaw) = turn_yaw(start.options) {
        camera_update::enable_turn(yaw);
    }
    Ok(())
}
