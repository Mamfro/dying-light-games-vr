//! The first-person arms follow the controllers (`aim=hand_rig`, [`eng_chr::rig`] on DL2's
//! offsets): the player camera update calls the arms visual's camera-target callback
//! (`engine::FPP_CAMERA_TARGET`) right after setting the camera; around it the arms are snapshot as
//! animated, then put on the controllers, and melee swings attack ([`eng_chr::fpp`]).
//!
//! `probe_hands=1` (`research::Options`) logs the skeleton's elements (name, type, parent, place
//! against the camera) and the weapon visuals' classes.

use crate::engine;
use crate::{look, game, stereo::head, stereo::scene};
use eng_chr::fpp::CameraTargetFn;
use eng_chr::rig::{Facts, Options, Rig};
use monaka_hook::module::Module;
use monaka_hook::{InFlight, Original};
use monaka_producer::Rejection;

pub static CAMERA_TARGET: Original<CameraTargetFn> = Original::new();

pub static RIG: Rig = Rig::new(Facts {
    callback: engine::FPP_CAMERA_TARGET,
    icamera_target: engine::FPP_ICAMERA_TARGET,
    skeleton: engine::SKELETON_LAYOUT,
    arms: engine::ARMS_LAYOUT,
    bones: engine::BONE_AXES,
    camera_key_at: None,
});

/// Installs the arms hook when the engine has the skeleton access and the vis vtable slot names the
/// expected callback.
///
/// # Safety
/// `gamedll` is the inspected build (hash checked by the caller).
pub unsafe fn install(hooks: &mut monaka_hook::Hooks, engine_module: &Module, gamedll: &Module, options: Options) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee; the detour has the callback's type.
    unsafe { RIG.install(hooks, engine_module, gamedll, options, &CAMERA_TARGET, camera_target) }
}

/// `PlayerFppVis_PH`'s camera-target callback `(ICameraTarget base, camera)`.
unsafe extern "system" fn camera_target(target: usize, camera: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET.get()(target, camera) };
    let counter = game::get().counter().map(|(_, c)| c as u64);
    RIG.around(&look::AIM, target, camera, scene::vr_active(), head::HEAD.current(), counter, original);
}

pub fn report() {
    RIG.report();
}
