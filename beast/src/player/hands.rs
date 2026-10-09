//! The first-person arms follow the controllers (`aim=hand_rig`, [`eng_chr::rig`] on The Beast's
//! offsets), as in Dying Light 2: the player camera update calls the arms visual's camera-target
//! callback (`engine::FPP_CAMERA_TARGET`) right after setting the camera; around it the arms are
//! snapshot as animated, then put on the controllers, and melee swings attack ([`eng_chr::fpp`]).
//! The camera the callback gets is already the eye's (`stereo`); the arms go against the game's
//! own camera, noted before the eye replaced it ([`aim::note_camera`], keyed by the renderer's
//! camera behind the game's).
//!
//! Head aim steers the character from here too ([`aim::steer`]): the callback gets the arms visual,
//! which leads to the character (the camera setter only sees the renderer's camera).
//!
//! Its probe (`probe_hands`: the skeleton's elements and the weapon visuals' classes, logged by
//! [`eng_chr::rig`]) takes its flag from `research::options()`.

use crate::player::aim;
use crate::engine;
use eng_chr::fpp::CameraTargetFn;
pub use eng_chr::rig::Options as Settings;
use eng_chr::rig::{Facts, Rig};
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
    fingers: engine::FINGER_AXES,
    camera_key_at: Some(engine::CAMERA_RENDERER),
});

/// Installs the arms hook when the engine has the skeleton access and the vis vtable slot names the
/// function measured.
///
/// # Safety
/// `gamedll` is the inspected build (hash checked by the caller).
pub unsafe fn install(hooks: &mut monaka_hook::Hooks, engine_module: &Module, gamedll: &Module, settings: Settings) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee; the detour has the callback's type.
    unsafe { RIG.install(hooks, engine_module, gamedll, settings, &CAMERA_TARGET, camera_target) }
}

/// `PlayerFppVis_PH`'s camera-target callback `(ICameraTarget base, camera)`.
unsafe extern "system" fn camera_target(target: usize, camera: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET.get()(target, camera) };
    let active = crate::view::stereo::running();
    let head = crate::view::stereo::HEAD.current();
    // Head aim steers here: this callback has the arms visual, and so the character.
    if active && aim::AIM.is_arms_target(target) {
        aim::steer(target, head);
    }
    RIG.around(&aim::AIM, target, camera, active, head, None, original);
}

pub fn report() {
    RIG.report();
}
