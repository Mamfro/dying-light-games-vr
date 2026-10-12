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
//! The callback is hooked for head aim ([`hook`]); the arms go on the controllers once the run's
//! options say so ([`configure`]: the rig, or its probe `probe_hands`, logging the skeleton's
//! elements and the weapon visuals' classes).

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

/// Hooks the arms visual's callback, once the vis vtable slot names it.
///
/// # Safety
/// `gamedll` is the inspected build (hash checked by the caller).
pub unsafe fn hook(hooks: &mut monaka_hook::Hooks, gamedll: &Module) -> Result<(), Rejection> {
    // SAFETY: the caller's guarantee; the detour has the callback's type.
    unsafe { RIG.hook(hooks, gamedll, &CAMERA_TARGET, camera_target) }
}

/// The arms on the controllers (or their probe), once the engine has the skeleton access.
pub fn configure(engine_module: &Module, settings: Settings) -> Result<(), Rejection> {
    RIG.configure(engine_module, settings)
}

/// `PlayerFppVis_PH`'s camera-target callback `(ICameraTarget base, camera)`.
unsafe extern "system" fn camera_target(target: usize, camera: usize) {
    let _flight = InFlight::enter();
    // SAFETY: forwards the game's own call.
    let original = || unsafe { CAMERA_TARGET.get()(target, camera) };
    let active = crate::view::stereo::running();
    let head = monaka_stereo::feature::DRIVER.head().current();
    // Head aim steers here: this callback has the arms visual, and so the character. Once stereo
    // stops, with no head, it hands the head's share back.
    if aim::AIM.is_arms_target(target) {
        aim::steer(target, head.filter(|_| active));
    }
    RIG.around(&aim::AIM, target, camera, active, head, None, original);
}

pub fn report() {
    RIG.report();
}
